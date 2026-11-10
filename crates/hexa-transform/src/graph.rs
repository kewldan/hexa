//! Node graph of transformations (XFRM-11).
//!
//! Nodes: `input` (the source data), `step` (one [`Step`]), `concat`
//! (joins its inputs in port order), `splitAt` (port 0 = head, port 1 =
//! tail) and `output`. Edges connect an output port of one node to an input
//! port of another. The graph must be a DAG.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::error::{Result, TransformError};
use crate::recipe::Step;

/// What a node does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum NodeKind {
    /// Emits the graph input on port 0.
    Input,
    /// Applies a step to port 0.
    Step { step: Step },
    /// Concatenates inputs 0..n in port order.
    Concat,
    /// Splits port 0 at `offset` (counted from the end with `fromEnd`);
    /// head on output port 0, tail on port 1. Offsets past the end clamp.
    SplitAt {
        offset: usize,
        #[serde(default)]
        from_end: bool,
    },
    /// A graph result, reported under the node id.
    Output {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl NodeKind {
    fn out_ports(&self) -> usize {
        match self {
            Self::Output { .. } => 0,
            Self::SplitAt { .. } => 2,
            _ => 1,
        }
    }
}

/// A node with a unique id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    #[serde(flatten)]
    pub kind: NodeKind,
}

/// A connection from `from`'s output port to `to`'s input port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from: String,
    #[serde(default)]
    pub from_port: usize,
    pub to: String,
    #[serde(default)]
    pub to_port: usize,
}

/// A transformation graph (serde, camelCase JSON).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

fn gerr(msg: impl Into<String>) -> TransformError {
    TransformError::Graph(msg.into())
}

impl Graph {
    /// Parses graph JSON.
    pub fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// Serialises to pretty JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Validates structure and returns node indices in evaluation order.
    /// Checks unique ids, edge endpoints and ports, that every required
    /// input is connected exactly once, at least one output, and no cycles.
    pub fn validate(&self) -> Result<Vec<usize>> {
        let mut index = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.id.is_empty() {
                return Err(gerr("node id must not be empty"));
            }
            if index.insert(n.id.as_str(), i).is_some() {
                return Err(gerr(format!("duplicate node id '{}'", n.id)));
            }
            if let NodeKind::Step { step } = &n.kind {
                step.validate().map_err(|e| TransformError::Node {
                    id: n.id.clone(),
                    source: Box::new(e),
                })?;
            }
        }
        if !self
            .nodes
            .iter()
            .any(|n| matches!(n.kind, NodeKind::Output { .. }))
        {
            return Err(gerr("graph has no output node"));
        }
        let mut inputs: Vec<HashSet<usize>> = vec![HashSet::new(); self.nodes.len()];
        let mut succ: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        let mut indeg = vec![0usize; self.nodes.len()];
        for e in &self.edges {
            let &f = index
                .get(e.from.as_str())
                .ok_or_else(|| gerr(format!("edge from unknown node '{}'", e.from)))?;
            let &t = index
                .get(e.to.as_str())
                .ok_or_else(|| gerr(format!("edge to unknown node '{}'", e.to)))?;
            let fk = &self.nodes[f].kind;
            if e.from_port >= fk.out_ports() {
                return Err(gerr(format!(
                    "node '{}' has no output port {}",
                    e.from, e.from_port
                )));
            }
            match self.nodes[t].kind {
                NodeKind::Input => {
                    return Err(gerr(format!("input node '{}' cannot have inputs", e.to)));
                }
                NodeKind::Concat => {}
                _ if e.to_port != 0 => {
                    return Err(gerr(format!(
                        "node '{}' has no input port {}",
                        e.to, e.to_port
                    )));
                }
                _ => {}
            }
            if !inputs[t].insert(e.to_port) {
                return Err(gerr(format!(
                    "input port {} of node '{}' is connected twice",
                    e.to_port, e.to
                )));
            }
            succ[f].push(t);
            indeg[t] += 1;
        }
        for (i, n) in self.nodes.iter().enumerate() {
            let ports = &inputs[i];
            match n.kind {
                NodeKind::Input => {}
                NodeKind::Concat => {
                    if ports.is_empty() || (0..ports.len()).any(|p| !ports.contains(&p)) {
                        return Err(gerr(format!(
                            "concat node '{}' needs inputs on ports 0..n without gaps",
                            n.id
                        )));
                    }
                }
                _ => {
                    if ports.is_empty() {
                        return Err(gerr(format!("node '{}' has no input", n.id)));
                    }
                }
            }
        }
        // Kahn's algorithm
        let mut queue: VecDeque<usize> = (0..self.nodes.len()).filter(|&i| indeg[i] == 0).collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(i) = queue.pop_front() {
            order.push(i);
            for &s in &succ[i] {
                indeg[s] -= 1;
                if indeg[s] == 0 {
                    queue.push_back(s);
                }
            }
        }
        if order.len() != self.nodes.len() {
            let stuck: Vec<&str> = (0..self.nodes.len())
                .filter(|&i| indeg[i] > 0)
                .map(|i| self.nodes[i].id.as_str())
                .collect();
            return Err(gerr(format!(
                "graph has a cycle through {}",
                stuck.join(", ")
            )));
        }
        Ok(order)
    }
}

/// Evaluates the graph with `input` fed to every input node. Returns the
/// data arriving at each output node, keyed by node id.
pub fn evaluate(graph: &Graph, input: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let order = graph.validate()?;
    let index: HashMap<&str, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i))
        .collect();
    // incoming[node][to_port] = (from node, from port)
    let mut incoming: Vec<BTreeMap<usize, (usize, usize)>> =
        vec![BTreeMap::new(); graph.nodes.len()];
    for e in &graph.edges {
        incoming[index[e.to.as_str()]].insert(e.to_port, (index[e.from.as_str()], e.from_port));
    }
    let mut values: HashMap<(usize, usize), Vec<u8>> = HashMap::new();
    let mut outputs = BTreeMap::new();
    for i in order {
        let node = &graph.nodes[i];
        let arg = |port: usize| -> &[u8] {
            let src = incoming[i][&port];
            values.get(&src).map_or(&[][..], Vec::as_slice)
        };
        let node_err = |e| TransformError::Node {
            id: node.id.clone(),
            source: Box::new(e),
        };
        let produced: Vec<Vec<u8>> = match &node.kind {
            NodeKind::Input => vec![input.to_vec()],
            NodeKind::Step { step } => vec![step.apply(arg(0)).map_err(node_err)?],
            NodeKind::Concat => {
                let mut v = Vec::new();
                for &p in incoming[i].keys() {
                    v.extend_from_slice(arg(p));
                }
                vec![v]
            }
            NodeKind::SplitAt { offset, from_end } => {
                let d = arg(0);
                let at = if *from_end {
                    d.len().saturating_sub(*offset)
                } else {
                    (*offset).min(d.len())
                };
                vec![d[..at].to_vec(), d[at..].to_vec()]
            }
            NodeKind::Output { .. } => {
                outputs.insert(node.id.clone(), arg(0).to_vec());
                Vec::new()
            }
        };
        for (port, v) in produced.into_iter().enumerate() {
            values.insert((i, port), v);
        }
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Codec;
    use crate::types::HexBytes;

    fn node(id: &str, kind: NodeKind) -> Node {
        Node {
            id: id.into(),
            kind,
        }
    }

    fn edge(from: &str, from_port: usize, to: &str, to_port: usize) -> Edge {
        Edge {
            from: from.into(),
            from_port,
            to: to.into(),
            to_port,
        }
    }

    fn sample() -> Graph {
        // header (4 bytes) kept, body XOR-decoded, then joined; body also hex
        Graph {
            nodes: vec![
                node("in", NodeKind::Input),
                node(
                    "split",
                    NodeKind::SplitAt {
                        offset: 4,
                        from_end: false,
                    },
                ),
                node(
                    "xor",
                    NodeKind::Step {
                        step: Step::XorKey {
                            key: HexBytes(vec![0x20]),
                        },
                    },
                ),
                node("join", NodeKind::Concat),
                node(
                    "hex",
                    NodeKind::Step {
                        step: Step::Encode {
                            codec: Codec::Base16 { uppercase: false },
                        },
                    },
                ),
                node("out", NodeKind::Output { label: None }),
                node(
                    "out_hex",
                    NodeKind::Output {
                        label: Some("Body as hex".into()),
                    },
                ),
            ],
            edges: vec![
                edge("in", 0, "split", 0),
                edge("split", 1, "xor", 0),
                edge("split", 0, "join", 0),
                edge("xor", 0, "join", 1),
                edge("join", 0, "out", 0),
                edge("xor", 0, "hex", 0),
                edge("hex", 0, "out_hex", 0),
            ],
        }
    }

    #[test]
    fn evaluates_dag() {
        let out = evaluate(&sample(), b"HDR:HELLO").unwrap();
        assert_eq!(out["out"], b"HDR:hello");
        assert_eq!(out["out_hex"], b"68656c6c6f");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn split_from_end_and_clamp() {
        let mut g = sample();
        g.nodes[1].kind = NodeKind::SplitAt {
            offset: 2,
            from_end: true,
        };
        assert_eq!(evaluate(&g, b"abcXY").unwrap()["out"], b"abcxy");
        g.nodes[1].kind = NodeKind::SplitAt {
            offset: 100,
            from_end: false,
        };
        assert_eq!(evaluate(&g, b"abc").unwrap()["out"], b"abc");
    }

    #[test]
    fn json_roundtrip() {
        let g = sample();
        let json = g.to_json().unwrap();
        assert!(json.contains("\"kind\": \"splitAt\""));
        assert!(json.contains("\"fromPort\": 1"));
        assert_eq!(Graph::from_json(&json).unwrap(), g);
        let ui = r#"{"nodes":[{"id":"a","kind":"input"},{"id":"n","kind":"step","step":{"kind":"not"}},{"id":"o","kind":"output"}],
                     "edges":[{"from":"a","to":"n"},{"from":"n","to":"o"}]}"#;
        assert_eq!(
            evaluate(&Graph::from_json(ui).unwrap(), &[0x0F]).unwrap()["o"],
            [0xF0]
        );
    }

    #[test]
    fn rejects_bad_graphs() {
        let mut g = sample();
        g.edges.push(edge("join", 0, "split", 0));
        assert!(
            g.validate()
                .unwrap_err()
                .to_string()
                .contains("connected twice")
        );

        let mut g = sample();
        // cycle: xor -> join -> xor (replace the split->xor edge)
        g.edges.retain(|e| !(e.from == "split" && e.to == "xor"));
        g.edges.push(edge("join", 0, "xor", 0));
        assert!(g.validate().unwrap_err().to_string().contains("cycle"));

        let mut g = sample();
        g.edges.retain(|e| e.to != "out_hex");
        assert!(g.validate().unwrap_err().to_string().contains("no input"));

        let mut g = sample();
        g.edges.push(edge("split", 2, "out_hex", 0));
        assert!(g.validate().is_err());

        let mut g = sample();
        g.nodes.push(node("in", NodeKind::Input));
        assert!(g.validate().unwrap_err().to_string().contains("duplicate"));

        let mut g = sample();
        g.edges.push(edge("ghost", 0, "join", 2));
        assert!(g.validate().is_err());

        let mut g = sample();
        g.edges.retain(|e| !(e.to == "join" && e.to_port == 0));
        assert!(
            g.validate()
                .unwrap_err()
                .to_string()
                .contains("without gaps")
        );

        let g = Graph {
            nodes: vec![node("in", NodeKind::Input)],
            edges: vec![],
        };
        assert!(g.validate().unwrap_err().to_string().contains("no output"));

        let mut g = sample();
        g.edges.push(edge("hex", 0, "in", 0));
        assert!(g.validate().is_err());
    }

    #[test]
    fn step_errors_name_the_node() {
        let mut g = sample();
        g.nodes[4].kind = NodeKind::Step {
            step: Step::Decode {
                codec: Codec::Base16 { uppercase: false },
            },
        };
        let e = evaluate(&g, b"HDR:zz").unwrap_err();
        assert!(e.to_string().starts_with("node 'hex':"), "{e}");
    }
}
