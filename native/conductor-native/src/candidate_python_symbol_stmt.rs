use super::*;
macro_rules! form {
    ($this:expr, $kind:expr, $depth:expr, $( $name:expr => $value:expr ),* $(,)?) => {{
        let fields = vec![$(($name, $value)),*];
        $this.form($kind, fields, $depth)
    }};
}
impl Dump<'_> {
    pub(super) fn stmt(&mut self, value: &ast::Stmt, d: usize) -> Result<String, String> {
        Self::check_depth(d)?;
        use ast::Stmt as S;
        match value {
            S::FunctionDef(v) => self.function(v, d),
            S::ClassDef(v) => self.class(v, d),
            S::Return(v) => {
                let mut f = vec![];
                self.optional_expr(&mut f, "value", v.value.as_deref(), d)?;
                self.form("Return", f, d)
            }
            S::Delete(v) => form!(self,"Delete",d,"targets"=>self.exprs(&v.targets,d+1)?),
            S::Assign(v) => {
                form!(self,"Assign",d,"targets"=>self.exprs(&v.targets,d+1)?,"value"=>self.expr(&v.value,d+1)?)
            }
            S::AugAssign(v) => {
                form!(self,"AugAssign",d,"target"=>self.expr(&v.target,d+1)?,"op"=>format!("{:?}()",v.op),"value"=>self.expr(&v.value,d+1)?)
            }
            S::AnnAssign(v) => {
                let mut f = vec![
                    ("target", self.expr(&v.target, d + 1)?),
                    ("annotation", self.expr(&v.annotation, d + 1)?),
                ];
                self.optional_expr(&mut f, "value", v.value.as_deref(), d)?;
                f.push(("simple", if v.simple { "1" } else { "0" }.into()));
                self.form("AnnAssign", f, d)
            }
            S::For(v) => {
                form!(self,if v.is_async {"AsyncFor"} else {"For"},d,"target"=>self.expr(&v.target,d+1)?,"iter"=>self.expr(&v.iter,d+1)?,"body"=>self.stmts(&v.body,d+1)?,"orelse"=>self.stmts(&v.orelse,d+1)?)
            }
            S::While(v) => {
                form!(self,"While",d,"test"=>self.expr(&v.test,d+1)?,"body"=>self.stmts(&v.body,d+1)?,"orelse"=>self.stmts(&v.orelse,d+1)?)
            }
            S::If(v) => self.if_stmt(v, d),
            S::With(v) => self.with_stmt(v, d),
            S::Try(v) => self.try_stmt(v, d),
            S::Raise(v) => {
                let mut f = vec![];
                self.optional_expr(&mut f, "exc", v.exc.as_deref(), d)?;
                self.optional_expr(&mut f, "cause", v.cause.as_deref(), d)?;
                self.form("Raise", f, d)
            }
            S::Assert(v) => {
                let mut f = vec![("test", self.expr(&v.test, d + 1)?)];
                self.optional_expr(&mut f, "msg", v.msg.as_deref(), d)?;
                self.form("Assert", f, d)
            }
            S::Import(v) => {
                if v.is_lazy {
                    return Err("lazy imports are not Python 3.12".into());
                }
                let names = self.aliases(&v.names, d + 1)?;
                self.form("Import", vec![("names", names)], d)
            }
            S::ImportFrom(v) => self.import_from(v, d),
            S::Global(v) => {
                let names = self.names(&v.names, d + 1)?;
                self.form("Global", vec![("names", names)], d)
            }
            S::Nonlocal(v) => {
                let names = self.names(&v.names, d + 1)?;
                self.form("Nonlocal", vec![("names", names)], d)
            }
            S::Expr(v) => form!(self,"Expr",d,"value"=>self.expr(&v.value,d+1)?),
            S::Pass(_) => self.form("Pass", vec![], d),
            S::Break(_) => self.form("Break", vec![], d),
            S::Continue(_) => self.form("Continue", vec![], d),
            S::TypeAlias(v) => {
                let name = self.expr(&v.name, d + 1)?;
                let params = self.type_params(v.type_params.as_deref(), d + 1)?;
                let value = self.expr(&v.value, d + 1)?;
                self.form(
                    "TypeAlias",
                    vec![("name", name), ("type_params", params), ("value", value)],
                    d,
                )
            }
            S::Match(v) => self.match_stmt(v, d),
            S::IpyEscapeCommand(_) => Err("unsupported Python statement in symbol".into()),
        }
    }
    fn function(&mut self, v: &ast::StmtFunctionDef, d: usize) -> Result<String, String> {
        let name = self.ident(&v.name, d + 1)?;
        let args = self.parameters(Some(v.parameters.as_ref()), d + 1)?;
        let body = self.stmts(&v.body, d + 1)?;
        let decorators = self.decorators(&v.decorator_list, d + 1)?;
        let mut f = vec![
            ("name", name),
            ("args", args),
            ("body", body),
            ("decorator_list", decorators),
        ];
        self.optional_expr(&mut f, "returns", v.returns.as_deref(), d)?;
        f.push((
            "type_params",
            self.type_params(v.type_params.as_deref(), d + 1)?,
        ));
        self.form(
            if v.is_async {
                "AsyncFunctionDef"
            } else {
                "FunctionDef"
            },
            f,
            d,
        )
    }
    fn class(&mut self, v: &ast::StmtClassDef, d: usize) -> Result<String, String> {
        let name = self.ident(&v.name, d + 1)?;
        let bases = self.exprs(v.bases(), d + 1)?;
        let keywords = self.keywords(v.keywords(), d + 1)?;
        let body = self.stmts(&v.body, d + 1)?;
        let decorators = self.decorators(&v.decorator_list, d + 1)?;
        let type_params = self.type_params(v.type_params.as_deref(), d + 1)?;
        self.form(
            "ClassDef",
            vec![
                ("name", name),
                ("bases", bases),
                ("keywords", keywords),
                ("body", body),
                ("decorator_list", decorators),
                ("type_params", type_params),
            ],
            d,
        )
    }
    fn decorators(&mut self, values: &[ast::Decorator], d: usize) -> Result<String, String> {
        let mut out = Vec::new();
        for v in values {
            out.push(self.expr(&v.expression, d + 1)?);
        }
        self.list(out, d)
    }
    pub(super) fn parameters(
        &mut self,
        values: Option<&ast::Parameters>,
        d: usize,
    ) -> Result<String, String> {
        let mut pos = Vec::new();
        let mut args = Vec::new();
        let mut kwonly = Vec::new();
        let mut kw_defaults = Vec::new();
        let mut defaults = Vec::new();
        if let Some(v) = values {
            for p in &v.posonlyargs {
                pos.push(self.parameter(&p.parameter, d + 1)?);
                if let Some(x) = &p.default {
                    defaults.push(self.expr(x, d + 1)?);
                }
            }
            for p in &v.args {
                args.push(self.parameter(&p.parameter, d + 1)?);
                if let Some(x) = &p.default {
                    defaults.push(self.expr(x, d + 1)?);
                }
            }
            for p in &v.kwonlyargs {
                kwonly.push(self.parameter(&p.parameter, d + 1)?);
                kw_defaults.push(match &p.default {
                    Some(x) => self.expr(x, d + 1)?,
                    None => "None".into(),
                });
            }
        }
        let pos = self.list(pos, d + 1)?;
        let args = self.list(args, d + 1)?;
        let mut f = vec![("posonlyargs", pos), ("args", args)];
        if let Some(p) = values.and_then(|v| v.vararg.as_deref()) {
            f.push(("vararg", self.parameter(p, d + 1)?));
        }
        f.push(("kwonlyargs", self.list(kwonly, d + 1)?));
        f.push(("kw_defaults", self.list(kw_defaults, d + 1)?));
        if let Some(p) = values.and_then(|v| v.kwarg.as_deref()) {
            f.push(("kwarg", self.parameter(p, d + 1)?));
        }
        f.push(("defaults", self.list(defaults, d + 1)?));
        self.form("arguments", f, d)
    }
    fn parameter(&mut self, v: &ast::Parameter, d: usize) -> Result<String, String> {
        let mut f = vec![("arg", self.ident(&v.name, d + 1)?)];
        self.optional_expr(&mut f, "annotation", v.annotation.as_deref(), d)?;
        self.form("arg", f, d)
    }
    fn type_params(
        &mut self,
        values: Option<&ast::TypeParams>,
        d: usize,
    ) -> Result<String, String> {
        let mut out = Vec::new();
        if let Some(values) = values {
            for item in &values.type_params {
                let (kind, name, bound, default) = match item {
                    ast::TypeParam::TypeVar(v) => {
                        ("TypeVar", &v.name, v.bound.as_deref(), v.default.as_deref())
                    }
                    ast::TypeParam::TypeVarTuple(v) => {
                        ("TypeVarTuple", &v.name, None, v.default.as_deref())
                    }
                    ast::TypeParam::ParamSpec(v) => {
                        ("ParamSpec", &v.name, None, v.default.as_deref())
                    }
                };
                if default.is_some() {
                    return Err("type parameter defaults require Python 3.13".into());
                }
                let mut f = vec![("name", self.ident(name, d + 1)?)];
                self.optional_expr(&mut f, "bound", bound, d)?;
                out.push(self.form(kind, f, d + 1)?);
            }
        }
        self.list(out, d)
    }
    fn if_stmt(&mut self, v: &ast::StmtIf, d: usize) -> Result<String, String> {
        let mut orelse = Vec::new();
        for (offset, clause) in v.elif_else_clauses.iter().enumerate().rev() {
            Self::check_depth(d + offset + 1)?;
            let body = self.stmts(&clause.body, d + offset + 2)?;
            if let Some(test) = &clause.test {
                let test = self.expr(test, d + offset + 2)?;
                let rest = self.list(orelse, d + offset + 2)?;
                orelse = vec![self.form(
                    "If",
                    vec![("test", test), ("body", body), ("orelse", rest)],
                    d + offset + 1,
                )?];
            } else {
                if !orelse.is_empty() {
                    return Err("invalid elif ordering".into());
                }
                orelse = clause
                    .body
                    .iter()
                    .map(|v| self.stmt(v, d + offset + 2))
                    .collect::<Result<_, _>>()?;
            }
        }
        let test = self.expr(&v.test, d + 1)?;
        let body = self.stmts(&v.body, d + 1)?;
        let orelse = self.list(orelse, d + 1)?;
        self.form(
            "If",
            vec![("test", test), ("body", body), ("orelse", orelse)],
            d,
        )
    }
    fn with_stmt(&mut self, v: &ast::StmtWith, d: usize) -> Result<String, String> {
        let mut items = Vec::new();
        for item in &v.items {
            let mut f = vec![("context_expr", self.expr(&item.context_expr, d + 2)?)];
            self.optional_expr(
                &mut f,
                "optional_vars",
                item.optional_vars.as_deref(),
                d + 1,
            )?;
            items.push(self.form("withitem", f, d + 1)?);
        }
        let items = self.list(items, d + 1)?;
        let body = self.stmts(&v.body, d + 1)?;
        self.form(
            if v.is_async { "AsyncWith" } else { "With" },
            vec![("items", items), ("body", body)],
            d,
        )
    }
    fn try_stmt(&mut self, v: &ast::StmtTry, d: usize) -> Result<String, String> {
        let body = self.stmts(&v.body, d + 1)?;
        let mut handlers = Vec::new();
        for handler in &v.handlers {
            let ast::ExceptHandler::ExceptHandler(h) = handler;
            let mut f = vec![];
            self.optional_expr(&mut f, "type", h.type_.as_deref(), d + 1)?;
            if let Some(name) = &h.name {
                f.push(("name", self.ident(name, d + 1)?));
            }
            f.push(("body", self.stmts(&h.body, d + 1)?));
            handlers.push(self.form("ExceptHandler", f, d + 1)?);
        }
        let handlers = self.list(handlers, d + 1)?;
        let orelse = self.stmts(&v.orelse, d + 1)?;
        let finalbody = self.stmts(&v.finalbody, d + 1)?;
        self.form(
            if v.is_star { "TryStar" } else { "Try" },
            vec![
                ("body", body),
                ("handlers", handlers),
                ("orelse", orelse),
                ("finalbody", finalbody),
            ],
            d,
        )
    }
    fn aliases(&mut self, values: &[ast::Alias], d: usize) -> Result<String, String> {
        let mut out = Vec::new();
        for v in values {
            let mut f = vec![("name", self.ident(&v.name, d + 1)?)];
            if let Some(name) = &v.asname {
                f.push(("asname", self.ident(name, d + 1)?));
            }
            out.push(self.form("alias", f, d + 1)?);
        }
        self.list(out, d)
    }
    fn import_from(&mut self, v: &ast::StmtImportFrom, d: usize) -> Result<String, String> {
        if v.is_lazy {
            return Err("lazy imports are not Python 3.12".into());
        }
        let mut f = vec![];
        if let Some(module) = &v.module {
            f.push(("module", self.ident(module, d + 1)?));
        }
        f.push(("names", self.aliases(&v.names, d + 1)?));
        f.push(("level", v.level.to_string()));
        self.form("ImportFrom", f, d)
    }
    fn names(&mut self, values: &[ast::Identifier], d: usize) -> Result<String, String> {
        let mut out = Vec::new();
        for name in values {
            out.push(self.ident(name, d + 1)?);
        }
        self.list(out, d)
    }
    fn match_stmt(&mut self, value: &ast::StmtMatch, d: usize) -> Result<String, String> {
        let subject = self.expr(&value.subject, d + 1)?;
        let mut cases = Vec::new();
        for case in &value.cases {
            let pattern = self.pattern(&case.pattern, d + 2)?;
            let mut fields = vec![("pattern", pattern)];
            self.optional_expr(&mut fields, "guard", case.guard.as_deref(), d + 1)?;
            fields.push(("body", self.stmts(&case.body, d + 2)?));
            cases.push(self.form("match_case", fields, d + 1)?);
        }
        let cases = self.list(cases, d + 1)?;
        self.form("Match", vec![("subject", subject), ("cases", cases)], d)
    }
    fn patterns(&mut self, values: &[ast::Pattern], d: usize) -> Result<String, String> {
        let mut out = Vec::new();
        for value in values {
            out.push(self.pattern(value, d + 1)?);
        }
        self.list(out, d)
    }
    fn pattern(&mut self, value: &ast::Pattern, d: usize) -> Result<String, String> {
        Self::check_depth(d)?;
        use ast::Pattern as P;
        match value {
            P::MatchValue(v) => {
                let value = self.expr(&v.value, d + 1)?;
                self.form("MatchValue", vec![("value", value)], d)
            }
            P::MatchSingleton(v) => self.form(
                "MatchSingleton",
                vec![("value", format!("{:?}", v.value))],
                d,
            ),
            P::MatchSequence(v) => {
                let patterns = self.patterns(&v.patterns, d + 1)?;
                self.form("MatchSequence", vec![("patterns", patterns)], d)
            }
            P::MatchOr(v) => {
                let patterns = self.patterns(&v.patterns, d + 1)?;
                self.form("MatchOr", vec![("patterns", patterns)], d)
            }
            P::MatchMapping(v) => {
                let keys = self.exprs(&v.keys, d + 1)?;
                let patterns = self.patterns(&v.patterns, d + 1)?;
                let mut f = vec![("keys", keys), ("patterns", patterns)];
                if let Some(rest) = &v.rest {
                    f.push(("rest", self.ident(rest, d + 1)?));
                }
                self.form("MatchMapping", f, d)
            }
            P::MatchClass(v) => self.match_class(v, d),
            P::MatchStar(v) => {
                let mut f = vec![];
                if let Some(name) = &v.name {
                    f.push(("name", self.ident(name, d + 1)?));
                }
                self.form("MatchStar", f, d)
            }
            P::MatchAs(v) => {
                let mut f = vec![];
                if let Some(p) = &v.pattern {
                    f.push(("pattern", self.pattern(p, d + 1)?));
                }
                if let Some(name) = &v.name {
                    f.push(("name", self.ident(name, d + 1)?));
                }
                self.form("MatchAs", f, d)
            }
        }
    }
    fn match_class(&mut self, value: &ast::PatternMatchClass, d: usize) -> Result<String, String> {
        let cls = self.expr(&value.cls, d + 1)?;
        let patterns = self.patterns(&value.arguments.patterns, d + 1)?;
        let mut attrs = Vec::new();
        let mut values = Vec::new();
        for kw in &value.arguments.keywords {
            attrs.push(self.ident(&kw.attr, d + 1)?);
            values.push(self.pattern(&kw.pattern, d + 1)?);
        }
        let attrs = self.list(attrs, d + 1)?;
        let values = self.list(values, d + 1)?;
        self.form(
            "MatchClass",
            vec![
                ("cls", cls),
                ("patterns", patterns),
                ("kwd_attrs", attrs),
                ("kwd_patterns", values),
            ],
            d,
        )
    }
}
