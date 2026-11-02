//! Finite YAML node graph. Alias edges preserve identity and never expand data.
#[path = "candidate_config_yaml_scalar.rs"]
mod scalar;
use super::{Budget, Limits};
use scalar::Tag;
use std::collections::{BTreeMap, BTreeSet};
use yaml_rust2::{
    parser::{Event, Parser},
    scanner::{Scanner, TokenType},
};
#[derive(Debug)]
enum Data {
    Scalar(String),
    Sequence(Vec<usize>),
    Mapping(Vec<usize>),
}
#[derive(Debug)]
struct Node {
    tag: Tag,
    data: Data,
}
struct Graph {
    nodes: Vec<Node>,
    root: Option<usize>,
    parents: Vec<usize>,
    anchors: BTreeMap<usize, usize>,
    budget: Budget,
}
fn charge(steps: &mut usize, limits: Limits) -> Result<(), &'static str> {
    *steps = steps
        .checked_add(1)
        .ok_or("YAML constructor work overflow")?;
    if *steps > limits.nodes.saturating_mul(16) {
        return Err("YAML constructor work limit exceeded");
    }
    Ok(())
}
fn anchor_names(text: &str, limits: Limits) -> Result<(), &'static str> {
    let mut scanner = Scanner::new(text.chars());
    let mut names = BTreeSet::new();
    let maximum = limits
        .nodes
        .checked_mul(6)
        .and_then(|n| n.checked_add(16))
        .ok_or("YAML token budget overflow")?;
    for _ in 0..maximum {
        let Some(token) = scanner.next() else {
            return if scanner.get_error().is_none() {
                Ok(())
            } else {
                Err("invalid YAML syntax")
            };
        };
        match token.1 {
            TokenType::Anchor(name) => {
                if names.len() >= 4096 || name.len() > limits.scalar || !names.insert(name) {
                    return Err("duplicate or excessive YAML anchor");
                }
            }
            TokenType::Alias(name) if !names.contains(&name) => return Err("undefined YAML alias"),
            _ => {}
        }
    }
    Err("YAML token limit exceeded")
}
impl Graph {
    fn edge(&mut self, id: usize) -> Result<(), &'static str> {
        if let Some(parent) = self.parents.last() {
            match &mut self.nodes[*parent].data {
                Data::Sequence(values) | Data::Mapping(values) => values.push(id),
                _ => return Err("invalid YAML parent kind"),
            }
        } else if self.root.replace(id).is_some() {
            return Err("multiple YAML roots");
        }
        Ok(())
    }
    fn add(&mut self, node: Node, anchor: usize) -> Result<usize, &'static str> {
        self.budget
            .node(self.parents.len() + usize::from(!matches!(&node.data, Data::Scalar(_))))?;
        let id = self.nodes.len();
        self.nodes.push(node);
        if anchor != 0 && self.anchors.insert(anchor, id).is_some() {
            return Err("duplicate YAML anchor ID");
        }
        self.edge(id)?;
        Ok(id)
    }
    fn event(&mut self, event: Event) -> Result<(), &'static str> {
        match event {
            Event::Alias(anchor) => {
                self.budget.node(self.parents.len())?;
                let id = *self.anchors.get(&anchor).ok_or("undefined YAML alias")?;
                self.edge(id)?;
            }
            Event::Scalar(text, style, anchor, tag) => {
                self.budget.scalar(text.len())?;
                let tag = scalar::resolve(tag, Some((&text, style)), Tag::Str)?;
                self.add(
                    Node {
                        tag,
                        data: Data::Scalar(text),
                    },
                    anchor,
                )?;
            }
            Event::SequenceStart(anchor, tag) => {
                let tag = scalar::resolve(tag, None, Tag::Seq)?;
                let id = self.add(
                    Node {
                        tag,
                        data: Data::Sequence(Vec::new()),
                    },
                    anchor,
                )?;
                self.parents.push(id);
            }
            Event::MappingStart(anchor, tag) => {
                let tag = scalar::resolve(tag, None, Tag::Map)?;
                let id = self.add(
                    Node {
                        tag,
                        data: Data::Mapping(Vec::new()),
                    },
                    anchor,
                )?;
                self.parents.push(id);
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let id = self.parents.pop().ok_or("unexpected YAML container end")?;
                match (&event, &self.nodes[id].data) {
                    (Event::SequenceEnd, Data::Sequence(_)) => {}
                    (Event::MappingEnd, Data::Mapping(values)) if values.len() % 2 == 0 => {}
                    _ => return Err("invalid or incomplete YAML container"),
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn parse(text: &str, limits: Limits) -> Result<Self, &'static str> {
        anchor_names(text, limits)?;
        let mut graph = Self {
            nodes: Vec::new(),
            root: None,
            parents: Vec::new(),
            anchors: BTreeMap::new(),
            budget: Budget {
                limits,
                nodes: 0,
                scalars: 0,
            },
        };
        let mut parser = Parser::new_from_str(text);
        let mut documents = 0;
        let maximum = limits
            .nodes
            .checked_mul(2)
            .and_then(|n| n.checked_add(8))
            .ok_or("YAML event budget overflow")?;
        for _ in 0..maximum {
            let (event, _) = parser.next_token().map_err(|_| "invalid YAML syntax")?;
            match event {
                Event::StreamEnd => {
                    return if graph.parents.is_empty() {
                        Ok(graph)
                    } else {
                        Err("incomplete YAML graph")
                    }
                }
                Event::DocumentStart => {
                    documents += 1;
                    if documents > 1 {
                        return Err("multiple YAML documents are unsupported");
                    }
                }
                _ => graph.event(event)?,
            }
        }
        Err("YAML event limit exceeded")
    }
    fn scalar_text(&self, mut id: usize, steps: &mut usize) -> Result<&str, &'static str> {
        let mut visited = BTreeSet::new();
        for _ in 0..=self.budget.limits.depth {
            charge(steps, self.budget.limits)?;
            if !visited.insert(id) {
                return Err("recursive YAML scalar constructor");
            }
            match &self.nodes[id].data {
                Data::Scalar(text) => return Ok(text),
                Data::Mapping(values) => {
                    let mut target = None;
                    for pair in values.as_chunks::<2>().0 {
                        charge(steps, self.budget.limits)?;
                        if self.nodes[pair[0]].tag == Tag::Value {
                            target = Some(pair[1]);
                            break;
                        }
                    }
                    id = target.ok_or("YAML scalar constructor requires scalar/value node")?;
                }
                _ => return Err("YAML scalar constructor requires scalar node"),
            }
        }
        Err("YAML scalar constructor depth exceeded")
    }
}
struct Validation<'a> {
    graph: &'a Graph,
    queue: Vec<usize>,
    maps: Vec<usize>,
    constructed: BTreeSet<usize>,
    flattened: BTreeSet<usize>,
    value_keys: BTreeSet<usize>,
    steps: usize,
}
impl Validation<'_> {
    fn enqueue(&mut self, id: usize, mapping: bool) -> Result<(), &'static str> {
        self.work()?;
        if self.queue.len() + self.maps.len() >= self.graph.budget.limits.nodes.saturating_mul(2) {
            return Err("YAML constructor queue limit exceeded");
        }
        if mapping {
            self.maps.push(id);
        } else {
            self.queue.push(id);
        }
        Ok(())
    }
    fn work(&mut self) -> Result<(), &'static str> {
        charge(&mut self.steps, self.graph.budget.limits)
    }
    fn map(&mut self, id: usize) -> Result<(), &'static str> {
        if !self.flattened.insert(id) {
            return Ok(());
        }
        let graph = self.graph;
        let Data::Mapping(values) = &graph.nodes[id].data else {
            return Err("YAML merge/map requires mapping node");
        };
        for pair in values.as_chunks::<2>().0 {
            self.work()?;
            if graph.nodes[pair[0]].tag == Tag::Merge {
                match &graph.nodes[pair[1]].data {
                    Data::Mapping(_) => self.enqueue(pair[1], true)?,
                    Data::Sequence(values) => {
                        for id in values {
                            self.work()?;
                            if !matches!(&graph.nodes[*id].data, Data::Mapping(_)) {
                                return Err("YAML merge sequence requires mappings");
                            }
                            self.enqueue(*id, true)?;
                        }
                    }
                    _ => return Err("YAML merge requires mapping or sequence of mappings"),
                }
            } else {
                let key = graph.nodes[pair[0]].tag;
                if matches!(key, Tag::Seq | Tag::Map | Tag::Set | Tag::Omap | Tag::Pairs) {
                    return Err("unhashable YAML mapping key");
                }
                if key != Tag::Value {
                    self.enqueue(pair[0], false)?;
                } else {
                    graph.scalar_text(pair[0], &mut self.steps)?;
                    // SafeConstructor turns a mapping's '=' key into a string;
                    // aliases referring to that exact node see the same change.
                    self.value_keys.insert(pair[0]);
                }
                self.enqueue(pair[1], false)?;
            }
        }
        Ok(())
    }
    fn construct(&mut self, id: usize) -> Result<(), &'static str> {
        if !self.constructed.insert(id) {
            return Ok(());
        }
        let graph = self.graph;
        let node = &graph.nodes[id];
        let tag = if node.tag == Tag::Value && self.value_keys.contains(&id) {
            Tag::Str
        } else {
            node.tag
        };
        match tag {
            Tag::Seq => {
                let Data::Sequence(values) = &node.data else {
                    return Err("YAML seq tag requires sequence");
                };
                for id in values {
                    self.enqueue(*id, false)?;
                }
            }
            Tag::Map | Tag::Set => self.enqueue(id, true)?,
            Tag::Omap | Tag::Pairs => {
                let Data::Sequence(values) = &node.data else {
                    return Err("YAML ordered map/pairs requires sequence");
                };
                for id in values {
                    self.work()?;
                    let Data::Mapping(pair) = &graph.nodes[*id].data else {
                        return Err("YAML ordered map/pairs requires single-entry mappings");
                    };
                    if pair.len() != 2 {
                        return Err("YAML ordered map/pairs requires single-entry mappings");
                    }
                    self.enqueue(pair[0], false)?;
                    self.enqueue(pair[1], false)?;
                }
            }
            Tag::Merge | Tag::Value => {
                return Err("YAML merge/value tag has no standalone safe constructor")
            }
            tag => {
                if tag == Tag::Timestamp && !matches!(&node.data, Data::Scalar(_)) {
                    return Err("YAML timestamp requires scalar");
                }
                scalar::validate(tag, graph.scalar_text(id, &mut self.steps)?)?;
            }
        }
        Ok(())
    }
    fn run(mut self) -> Result<(), &'static str> {
        let Some(root) = self.graph.root else {
            return Ok(());
        };
        self.enqueue(root, false)?;
        while !self.queue.is_empty() || !self.maps.is_empty() {
            self.work()?;
            if self.queue.len() + self.maps.len() > self.graph.budget.limits.nodes.saturating_mul(2)
            {
                return Err("YAML constructor queue limit exceeded");
            }
            if let Some(id) = self.queue.pop() {
                self.construct(id)?;
            } else if let Some(id) = self.maps.pop() {
                self.map(id)?;
            }
        }
        Ok(())
    }
}
pub(super) fn validate(text: &str, limits: Limits) -> Result<(), &'static str> {
    let graph = Graph::parse(text, limits)?;
    Validation {
        graph: &graph,
        queue: Vec::new(),
        maps: Vec::new(),
        constructed: BTreeSet::new(),
        flattened: BTreeSet::new(),
        value_keys: BTreeSet::new(),
        steps: 0,
    }
    .run()
}
#[cfg(test)]
#[path = "candidate_config_yaml_tests.rs"]
mod tests;
