"""Ablation rules: neuter one construct to identity, leave everything else alone.

A mutation-testing patch asks "if I corrupt this, does a test notice?". An ablation
asks the narrower question this module exists for: "if I simply REMOVE this, does
anything change at all?". Removal is the right operator for finding decorative code,
because a construct that can be deleted with no observable effect is not doing
anything -- whereas a corrupted constant may merely be untested.

Every rule here is syntactic and local. Rules are deliberately conservative: they
fire only on shapes whose identity element is unambiguous, because a rule that
changes meaning turns a NO_DIFFERENCE verdict into a lie.
"""

from __future__ import annotations

import ast
import copy
from dataclasses import dataclass
from typing import Callable, Iterator, Sequence

__all__ = ["Ablation", "generate_ablations", "RULES"]

_NORM_METHODS = frozenset({"norm", "std", "rms", "sum", "abs"})
_DROPPABLE_METHODS = frozenset(
    {"clamp", "clamp_min", "clamp_max", "masked_fill", "masked_fill_", "detach",
     "tril", "triu", "nan_to_num", "contiguous", "sigmoid_", "relu_"}
)


@dataclass(frozen=True)
class Ablation:
    """One neutered variant of a function, ready to compile and compare."""

    rule: str
    description: str
    lineno: int
    tree: ast.FunctionDef | ast.AsyncFunctionDef

    @property
    def ident(self) -> str:
        return f"{self.rule}@L{self.lineno}"


def _replace(tree: ast.AST, target: ast.AST, replacement: ast.AST) -> ast.AST:
    """Return a copy of ``tree`` with the node at ``target``'s position swapped."""

    class _Swap(ast.NodeTransformer):
        def generic_visit(self, node: ast.AST) -> ast.AST:
            if getattr(node, "_ablate_here", False):
                return replacement
            return super().generic_visit(node)

    marked = copy.deepcopy(tree)
    for node in ast.walk(marked):
        if _same_position(node, target) and type(node) is type(target):
            node._ablate_here = True  # type: ignore[attr-defined]
            break
    else:
        raise LookupError("ablation target vanished from the copied tree")
    return ast.fix_missing_locations(_Swap().visit(marked))


def _same_position(a: ast.AST, b: ast.AST) -> bool:
    fields = ("lineno", "col_offset", "end_lineno", "end_col_offset")
    return all(getattr(a, f, None) == getattr(b, f, object()) for f in fields)


def _drop_statement(tree: ast.AST, target: ast.stmt) -> ast.AST:
    class _Drop(ast.NodeTransformer):
        def generic_visit(self, node: ast.AST) -> ast.AST:
            for field, value in list(ast.iter_fields(node)):
                if isinstance(value, list):
                    kept = [v for v in value
                            if not (isinstance(v, ast.stmt) and _same_position(v, target))]
                    if len(kept) != len(value):
                        setattr(node, field, kept or [ast.Pass()])
            return super().generic_visit(node)

    return ast.fix_missing_locations(_Drop().visit(copy.deepcopy(tree)))


def _rule_drop_method(fn: ast.AST) -> Iterator[tuple[str, str, ast.AST, ast.AST]]:
    """``x.clamp(...)`` / ``x.detach()`` / ``x.masked_fill(...)`` -> ``x``."""
    for node in ast.walk(fn):
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                and node.func.attr in _DROPPABLE_METHODS):
            yield (f"drop_{node.func.attr}",
                   f".{node.func.attr}(...) removed", node, node.func.value)


def _rule_drop_normalisation(fn: ast.AST) -> Iterator[tuple[str, str, ast.AST, ast.AST]]:
    """``x / x.norm(...)`` -> ``x``. Only when the denominator is a magnitude."""
    for node in ast.walk(fn):
        if not (isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div)):
            continue
        if _mentions_magnitude(node.right):
            yield ("drop_normalisation", "division by a magnitude removed",
                   node, node.left)


def _mentions_magnitude(node: ast.AST) -> bool:
    for sub in ast.walk(node):
        if isinstance(sub, ast.Attribute) and sub.attr in _NORM_METHODS:
            return True
        if isinstance(sub, ast.Name) and sub.id in {"norm", "denom", "scale", "rms"}:
            return True
    return False


def _rule_drop_where(fn: ast.AST) -> Iterator[tuple[str, str, ast.AST, ast.AST]]:
    """``torch.where(cond, a, b)`` -> ``a``: the guarded branch always taken."""
    for node in ast.walk(fn):
        if (isinstance(node, ast.Call) and len(node.args) == 3
                and isinstance(node.func, ast.Attribute) and node.func.attr == "where"):
            yield ("drop_where", "torch.where(...) collapsed to its first branch",
                   node, node.args[0 + 1])


def _rule_drop_raise_guard(fn: ast.AST) -> Iterator[tuple[str, str, ast.stmt, None]]:
    """``if cond: raise ...`` removed -- a validation guard that may never fire."""
    for node in ast.walk(fn):
        if (isinstance(node, ast.If) and not node.orelse and len(node.body) == 1
                and isinstance(node.body[0], ast.Raise)):
            yield ("drop_raise_guard", "validation guard removed", node, None)


def _rule_drop_trailing_arg(
    fn: ast.AST, defaulted: frozenset[str] = frozenset()
) -> Iterator[tuple[str, str, ast.AST, ast.AST]]:
    """``f(a, guard)`` -> ``f(a)``: an optional guard argument left at its default.

    This is the shape that hides real work behind an argument nobody varies -- a mask
    passed to a helper that also accepts ``None``. It fires ONLY for callees resolved
    in the same module whose trailing parameter has a default, because without that
    check the rule matches every two-argument call in the file -- ``isinstance``
    included -- and buries one real finding under dozens of meaningless ones.
    """
    for node in ast.walk(fn):
        if not (isinstance(node, ast.Call) and len(node.args) >= 2 and not node.keywords):
            continue
        if _callee_name(node) not in defaulted:
            continue
        trimmed = copy.deepcopy(node)
        trimmed.args = trimmed.args[:-1]
        yield ("drop_trailing_arg",
               f"trailing argument dropped from {_callee_name(node)}(...)",
               node, trimmed)


def _callee_name(call: ast.Call) -> str:
    if isinstance(call.func, ast.Name):
        return call.func.id
    if isinstance(call.func, ast.Attribute):
        return call.func.attr
    return "<call>"


def _rule_flip_boolean_default(
    fn: ast.AST, _defaulted: frozenset[str] = frozenset()
) -> Iterator[tuple[str, str, ast.AST, ast.AST]]:
    """``def f(..., flag=True)`` -> ``flag=False``: is the default load-bearing?

    Two different findings share this shape. If every caller passes the argument
    explicitly the flip is invisible and the default is dead weight. If no caller
    ever overrides it, the flip changes everything and the OTHER branch is what is
    untested. Both are worth knowing and neither is visible from the call sites.
    """
    if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return
    for default in list(fn.args.defaults) + [d for d in fn.args.kw_defaults if d]:
        if isinstance(default, ast.Constant) and isinstance(default.value, bool):
            flipped = ast.Constant(value=not default.value)
            yield ("flip_boolean_default",
                   f"default {default.value} flipped to {not default.value}",
                   default, flipped)


def _rule_drop_unary_call(
    fn: ast.AST, unary: frozenset[str] = frozenset()
) -> Iterator[tuple[str, str, ast.AST, ast.AST]]:
    """``f(x)`` -> ``x`` for a one-argument helper defined in the same module.

    The general form of "is this transformation doing anything". Restricted to
    single-argument module-level callees so the substitution is type-preserving by
    construction; dropping an arbitrary call changes the shape of the value and
    reports a difference that says nothing.
    """
    for node in ast.walk(fn):
        if (isinstance(node, ast.Call) and len(node.args) == 1 and not node.keywords
                and isinstance(node.func, ast.Name) and node.func.id in unary):
            yield ("drop_unary_call", f"{node.func.id}(...) replaced by its argument",
                   node, node.args[0])


def _rule_drop_expression_statement(
    fn: ast.AST, _u: frozenset[str] = frozenset()
) -> Iterator[tuple[str, str, ast.stmt, None]]:
    """A bare ``foo(x)`` statement removed: does this side effect matter?

    A call whose result is discarded is there purely for its effect. Removing it is
    the procedure-level equivalent of ablating a value: if nothing downstream moves,
    either the effect is inert or nothing observes it.
    """
    for node in ast.walk(fn):
        if (isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
                and not isinstance(node.value.func, ast.Attribute)):
            yield ("drop_expression_statement",
                   f"discarded call to {_callee_name(node.value)}(...) removed",
                   node, None)
        elif (isinstance(node, ast.Expr) and isinstance(node.value, ast.Call)
              and isinstance(node.value.func, ast.Attribute)
              and node.value.func.attr.endswith("_")):
            yield ("drop_expression_statement",
                   f"discarded in-place {node.value.func.attr}(...) removed",
                   node, None)


def _rule_body_to_passthrough(
    fn: ast.AST, _u: frozenset[str] = frozenset()
) -> Iterator[tuple[str, str, ast.AST, ast.AST]]:
    """Replace the whole body with ``return <first argument>``.

    The bluntest question there is: does this function earn its existence, or is it
    an identity wearing a name? Only fires for a function that takes an argument to
    return, and never for one whose body is already a single statement.
    """
    if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return
    positional = [a.arg for a in fn.args.args if a.arg not in ("self", "cls")]
    if not positional or len(fn.body) < 2:
        return
    stub = copy.deepcopy(fn)
    stub.decorator_list = []
    stub.body = [ast.Return(value=ast.Name(id=positional[0], ctx=ast.Load()))]
    ast.fix_missing_locations(stub)
    yield ("body_to_passthrough",
           f"entire body replaced by `return {positional[0]}`",
           fn, stub)


RULES: tuple[Callable[..., Iterator[tuple]], ...] = (
    _rule_drop_method,
    _rule_drop_normalisation,
    _rule_drop_where,
    _rule_drop_raise_guard,
    _rule_drop_trailing_arg,
    _rule_flip_boolean_default,
    _rule_drop_unary_call,
    _rule_drop_expression_statement,
)

# Rules that fire often and have not yet been shown to DISCRIMINATE: measured over 26
# modules, body_to_passthrough produced 72 LIVE verdicts and no non-LIVE verdict at all
# while accounting for 44.6% of the sweep's ablations. Most functions are not identities,
# so that is what it should look like -- but until it finds one it is cost without
# signal, and paying it by default makes every other rule slower to reach.
OPTIONAL_RULES: dict[str, Callable[..., Iterator[tuple]]] = {
    "body_to_passthrough": _rule_body_to_passthrough,
}

# Rules that need the module's own definitions to stay type-safe or meaningful.
_MODULE_AWARE = {"_rule_drop_trailing_arg": "defaulted", "_rule_drop_unary_call": "unary"}


def defaulted_callees(module_tree: ast.AST | None) -> frozenset[str]:
    """Names in this module whose last parameter has a default, so dropping it is legal."""
    if module_tree is None:
        return frozenset()
    names = set()
    for node in ast.walk(module_tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.args.defaults:
            names.add(node.name)
    return frozenset(names)


def unary_callees(module_tree: ast.AST | None) -> frozenset[str]:
    """Module-level names taking exactly one required argument.

    Replacing ``f(x)`` with ``x`` is only type-preserving for this shape; for anything
    else the substitution changes what the value IS, and the difference it reports is
    an artifact of the rule rather than a fact about the code.
    """
    if module_tree is None:
        return frozenset()
    names = set()
    for node in getattr(module_tree, "body", []):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        a = node.args
        if (len(a.args) == 1 and not a.defaults and not a.kwonlyargs
                and a.vararg is None and a.kwarg is None):
            names.add(node.name)
    return frozenset(names)


def generate_ablations(
    fn: ast.FunctionDef | ast.AsyncFunctionDef, module_tree: ast.AST | None = None,
    extra_rules: Sequence[str] = (),
) -> list[Ablation]:
    """Every single-construct ablation of ``fn``, in source order.

    ``extra_rules`` names entries of ``OPTIONAL_RULES`` to switch on; an unknown name
    is an error rather than a silent no-op, because a probe that quietly ran fewer
    rules than asked would report a clean sweep it never performed.
    """
    unknown = set(extra_rules) - set(OPTIONAL_RULES)
    if unknown:
        raise KeyError(f"unknown ablation rule(s): {sorted(unknown)}")
    active = tuple(RULES) + tuple(OPTIONAL_RULES[name] for name in extra_rules)
    extra = {
        _rule_drop_trailing_arg: defaulted_callees(module_tree),
        _rule_drop_unary_call: unary_callees(module_tree),
    }
    out: list[Ablation] = []
    for rule in active:
        if rule in extra:
            args: tuple = (fn, extra[rule])
        elif rule in (_rule_flip_boolean_default, _rule_drop_expression_statement,
                      _rule_body_to_passthrough):
            args = (fn, frozenset())
        else:
            args = (fn,)
        for name, description, target, replacement in rule(*args):
            try:
                tree = (_drop_statement(fn, target) if replacement is None
                        else _replace(fn, target, replacement))
            except LookupError:
                continue
            out.append(Ablation(name, description, getattr(target, "lineno", 0), tree))
    out.sort(key=lambda a: (a.lineno, a.rule))
    return out
