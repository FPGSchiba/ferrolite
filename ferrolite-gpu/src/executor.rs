//! A generic, photo-agnostic retained-DAG executor: nodes produce outputs `O`,
//! edges declare inputs, dirty flags drive minimal recompute with cached
//! outputs. Knows nothing about images, tiles, or wgpu (cross-cutting
//! contract §4); Spec 2's photo edit nodes implement `Node` and slot in.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

/// A unit of computation producing an output of type `O` from its inputs' outputs.
pub trait Node<O> {
    fn evaluate(&self, inputs: &[&O]) -> O;
}

struct Entry<O> {
    node: Box<dyn Node<O>>,
    inputs: Vec<NodeId>,
    cache: Option<O>,
    dirty: bool,
}

pub struct Graph<O> {
    nodes: Vec<Entry<O>>,
    eval_count: usize,
}

impl<O: Clone> Graph<O> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            eval_count: 0,
        }
    }

    pub fn add_node(&mut self, node: Box<dyn Node<O>>, inputs: Vec<NodeId>) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(Entry {
            node,
            inputs,
            cache: None,
            dirty: true,
        });
        id
    }

    /// Mark `id` dirty and transitively mark every node that depends on it.
    pub fn mark_dirty(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            if !self.nodes[cur.0].dirty {
                self.nodes[cur.0].dirty = true;
            }
            // Dependents: any node listing `cur` as an input.
            for i in 0..self.nodes.len() {
                if self.nodes[i].inputs.contains(&cur) && !self.nodes[i].dirty {
                    stack.push(NodeId(i));
                }
            }
        }
        // The seed itself must be dirty even if already false-skipped above.
        self.nodes[id.0].dirty = true;
    }

    /// Evaluate `id`, recursively evaluating dirty inputs; clean nodes return
    /// their cached output. Returns a reference into the cache.
    pub fn evaluate(&mut self, id: NodeId) -> &O {
        self.eval_recursive(id);
        self.nodes[id.0]
            .cache
            .as_ref()
            .expect("evaluated node has a cache")
    }

    pub fn eval_count(&self) -> usize {
        self.eval_count
    }

    fn eval_recursive(&mut self, id: NodeId) {
        if !self.nodes[id.0].dirty && self.nodes[id.0].cache.is_some() {
            return;
        }
        let input_ids = self.nodes[id.0].inputs.clone();
        for &inp in &input_ids {
            self.eval_recursive(inp);
        }
        let inputs: Vec<&O> = input_ids
            .iter()
            .map(|i| self.nodes[i.0].cache.as_ref().expect("input cached"))
            .collect();
        let out = self.nodes[id.0].node.evaluate(&inputs);
        self.eval_count += 1;
        let entry = &mut self.nodes[id.0];
        entry.cache = Some(out);
        entry.dirty = false;
    }
}

impl<O: Clone> Default for Graph<O> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Const(i64);
    impl Node<i64> for Const {
        fn evaluate(&self, _inputs: &[&i64]) -> i64 {
            self.0
        }
    }
    struct Add;
    impl Node<i64> for Add {
        fn evaluate(&self, inputs: &[&i64]) -> i64 {
            inputs.iter().copied().sum()
        }
    }

    #[test]
    fn evaluates_in_topological_order() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(2)), vec![]);
        let b = g.add_node(Box::new(Const(3)), vec![]);
        let sum = g.add_node(Box::new(Add), vec![a, b]);
        assert_eq!(*g.evaluate(sum), 5);
    }

    #[test]
    fn caches_clean_nodes_no_reevaluation() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(2)), vec![]);
        let b = g.add_node(Box::new(Const(3)), vec![]);
        let sum = g.add_node(Box::new(Add), vec![a, b]);
        assert_eq!(*g.evaluate(sum), 5);
        let after_first = g.eval_count();
        assert_eq!(*g.evaluate(sum), 5); // all clean -> cache hit
        assert_eq!(
            g.eval_count(),
            after_first,
            "no node re-evaluated when all clean"
        );
    }

    #[test]
    fn dirty_propagates_to_dependents() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(2)), vec![]);
        let b = g.add_node(Box::new(Const(3)), vec![]);
        let sum = g.add_node(Box::new(Add), vec![a, b]);
        assert_eq!(*g.evaluate(sum), 5);
        g.mark_dirty(a); // a and its dependent `sum` must re-evaluate; b must not
        let before = g.eval_count();
        assert_eq!(*g.evaluate(sum), 5);
        assert_eq!(g.eval_count(), before + 2, "only a and sum re-evaluate");
    }

    #[test]
    fn diamond_evaluates_shared_input_once() {
        // a -> b, a -> c, (b,c) -> d : evaluating d evaluates a exactly once.
        let mut g = Graph::new();
        let a = g.add_node(Box::new(Const(1)), vec![]);
        let b = g.add_node(Box::new(Add), vec![a]);
        let c = g.add_node(Box::new(Add), vec![a]);
        let d = g.add_node(Box::new(Add), vec![b, c]);
        assert_eq!(*g.evaluate(d), 2);
        assert_eq!(g.eval_count(), 4, "a,b,c,d each once");
    }
}
