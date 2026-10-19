//! Which commands carry a message argument, read from the command tree the
//! server sends (Plan.md P11.3; ADR-0020, S2).
//!
//! On a server that enforces secure chat, a command's message argument must
//! be signed, and azalea 0.16 signs no command (its chat handler's "TODO:
//! commands that require chat signing"), so the server would reject it. The
//! vanilla client decides what to sign from this same tree, and the server
//! checks against its own copy, so [`MessageCommands`] names exactly the
//! commands it would reject: a root literal that can reach a `message`
//! argument through children and redirects. A redirect to the root, as in
//! `/execute … run`, isn't followed: vanilla's own check stops there too.
//!
//! The tree is untrusted input. The names are marked in one pass backwards
//! from the message arguments, each node at most once, and an index out of
//! range is ignored, so a tree built to be slow costs one pass.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use azalea::protocol::packets::game::ClientboundCommands;
use azalea::protocol::packets::game::c_commands::{BrigadierParser, NodeType};

/// The commands with a message argument, by the names the server's tree
/// gives them (`me`, `msg`, `tell`, `minecraft:msg`, …). Empty until a tree
/// arrives, so commands go out as before then, as the vanilla client sends
/// them with an empty tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MessageCommands(Arc<BTreeSet<String>>);

impl MessageCommands {
    /// The commands of `tree` with a message argument.
    #[must_use]
    pub(crate) fn of(tree: &ClientboundCommands) -> Self {
        mark(tree).0
    }

    /// Whether the command `name` has a message argument.
    #[must_use]
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    /// The commands `names`, for tests.
    #[cfg(test)]
    pub(crate) fn from_names(names: &[&str]) -> Self {
        Self(Arc::new(
            names.iter().map(|&name| name.to_owned()).collect(),
        ))
    }
}

/// The commands of `tree` with a message argument, and how many nodes the
/// pass visited: each at most once.
fn mark(tree: &ClientboundCommands) -> (MessageCommands, usize) {
    let nodes = &tree.entries;
    let index = |raw: u32| usize::try_from(raw).ok().filter(|&at| at < nodes.len());
    let Some(root) = index(tree.root_index) else {
        return (MessageCommands::default(), 0);
    };
    // For each node, the nodes that lead to it: its parents, and the nodes
    // that redirect to it, unless it's the root.
    let mut leads_to: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (from, node) in nodes.iter().enumerate() {
        let redirect = node.redirect_node.and_then(index).filter(|&to| to != root);
        for to in node
            .children
            .iter()
            .copied()
            .filter_map(index)
            .chain(redirect)
        {
            if let Some(sources) = leads_to.get_mut(to) {
                sources.push(from);
            }
        }
    }
    let mut reaches = vec![false; nodes.len()];
    let mut queue = VecDeque::new();
    for (at, node) in nodes.iter().enumerate() {
        let message = matches!(
            node.node_type,
            NodeType::Argument {
                parser: BrigadierParser::Message,
                ..
            }
        );
        if message && let Some(seen) = reaches.get_mut(at) {
            *seen = true;
            queue.push_back(at);
        }
    }
    let mut visited = 0;
    while let Some(at) = queue.pop_front() {
        visited += 1;
        for &from in leads_to.get(at).into_iter().flatten() {
            if let Some(seen) = reaches.get_mut(from)
                && !*seen
            {
                *seen = true;
                queue.push_back(from);
            }
        }
    }
    let names = nodes
        .get(root)
        .into_iter()
        .flat_map(|root| root.children.iter().copied().filter_map(index))
        .filter(|&child| reaches.get(child).copied().unwrap_or(false))
        .filter_map(|child| match &nodes.get(child)?.node_type {
            NodeType::Literal { name } => Some(name.clone()),
            NodeType::Root | NodeType::Argument { .. } => None,
        })
        .collect();
    (MessageCommands(Arc::new(names)), visited)
}

#[cfg(test)]
mod tests {
    use azalea::protocol::packets::game::c_commands::{
        BrigadierNodeStub, BrigadierString, EntityParser,
    };
    use rstest::rstest;

    use super::*;

    fn node(
        node_type: NodeType,
        children: &[u32],
        redirect_node: Option<u32>,
    ) -> BrigadierNodeStub {
        BrigadierNodeStub {
            is_executable: children.is_empty(),
            children: children.to_vec(),
            redirect_node,
            node_type,
            is_restricted: false,
        }
    }

    fn root(children: &[u32]) -> BrigadierNodeStub {
        node(NodeType::Root, children, None)
    }

    fn literal(name: &str, children: &[u32]) -> BrigadierNodeStub {
        node(
            NodeType::Literal {
                name: name.to_owned(),
            },
            children,
            None,
        )
    }

    /// A literal that continues as node `to`.
    fn alias(name: &str, to: u32) -> BrigadierNodeStub {
        node(
            NodeType::Literal {
                name: name.to_owned(),
            },
            &[],
            Some(to),
        )
    }

    fn argument(name: &str, parser: BrigadierParser, children: &[u32]) -> BrigadierNodeStub {
        node(
            NodeType::Argument {
                name: name.to_owned(),
                parser,
                suggestions_type: None,
            },
            children,
            None,
        )
    }

    fn message() -> BrigadierNodeStub {
        argument("message", BrigadierParser::Message, &[])
    }

    fn targets(children: &[u32]) -> BrigadierNodeStub {
        argument(
            "targets",
            BrigadierParser::Entity(EntityParser {
                single: false,
                players_only: true,
            }),
            children,
        )
    }

    fn tree(root_index: u32, entries: Vec<BrigadierNodeStub>) -> ClientboundCommands {
        ClientboundCommands {
            entries,
            root_index,
        }
    }

    fn names(tree: &ClientboundCommands) -> Vec<String> {
        MessageCommands::of(tree).0.iter().cloned().collect()
    }

    /// The vanilla shapes: `me <action>`, `say <message>`, `teammsg
    /// <message>`, `msg <targets> <message>`, with `tell` and `w` redirecting
    /// to `msg` and `tm` to `teammsg`, and `spawn`, which has no message.
    fn vanilla() -> ClientboundCommands {
        tree(
            0,
            vec![
                root(&[1, 3, 5, 7, 10, 11, 12, 13]),
                literal("me", &[2]),
                argument("action", BrigadierParser::Message, &[]),
                literal("say", &[4]),
                message(),
                literal("teammsg", &[6]),
                message(),
                literal("msg", &[8]),
                targets(&[9]),
                message(),
                alias("tell", 7),
                alias("w", 7),
                alias("tm", 5),
                literal("spawn", &[]),
            ],
        )
    }

    #[test]
    fn the_vanilla_message_commands_and_their_aliases_are_named() {
        assert_eq!(
            names(&vanilla()),
            ["me", "msg", "say", "teammsg", "tell", "tm", "w"]
        );
    }

    #[test]
    fn a_namespaced_alias_is_named_as_the_tree_has_it() {
        let commands = tree(
            0,
            vec![
                root(&[1, 3]),
                literal("msg", &[2]),
                message(),
                alias("minecraft:msg", 1),
            ],
        );

        assert_eq!(names(&commands), ["minecraft:msg", "msg"]);
    }

    /// `execute … run` redirects to the root, which the vanilla check doesn't
    /// follow: the command it runs is checked on its own.
    #[test]
    fn a_redirect_to_the_root_isnt_followed() {
        let commands = tree(
            0,
            vec![
                root(&[1, 3]),
                literal("execute", &[2]),
                alias("run", 0),
                literal("me", &[4]),
                message(),
            ],
        );

        assert_eq!(names(&commands), ["me"]);
    }

    /// A plugin's `me` that reads a plain greedy string needs no signature.
    #[test]
    fn a_plain_text_argument_isnt_a_message() {
        let commands = tree(
            0,
            vec![
                root(&[1]),
                literal("me", &[2]),
                argument(
                    "text",
                    BrigadierParser::String(BrigadierString::GreedyPhrase),
                    &[],
                ),
            ],
        );

        assert_eq!(names(&commands), Vec::<String>::new());
    }

    #[test]
    fn a_redirect_cycle_ends_and_names_what_reaches_a_message() {
        let commands = tree(
            0,
            vec![
                root(&[1, 2, 3]),
                alias("a", 2),
                alias("b", 1),
                literal("c", &[4]),
                node(
                    NodeType::Argument {
                        name: "message".to_owned(),
                        parser: BrigadierParser::Message,
                        suggestions_type: None,
                    },
                    &[],
                    Some(3),
                ),
            ],
        );

        assert_eq!(names(&commands), ["c"]);
    }

    #[rstest]
    #[case::a_child_out_of_range(vec![root(&[1, 99]), literal("me", &[2, 1_000]), message()])]
    #[case::a_redirect_out_of_range(vec![
        root(&[1, 3]),
        literal("me", &[2]),
        message(),
        alias("far", 500),
    ])]
    #[case::the_largest_index(vec![root(&[1, u32::MAX]), literal("me", &[2]), message()])]
    fn an_index_out_of_range_is_ignored(#[case] entries: Vec<BrigadierNodeStub>) {
        assert_eq!(names(&tree(0, entries)), ["me"]);
    }

    #[rstest]
    #[case::past_the_end(3)]
    #[case::the_largest(u32::MAX)]
    fn a_root_out_of_range_names_nothing(#[case] root_index: u32) {
        let commands = tree(root_index, vec![root(&[1]), literal("me", &[2]), message()]);

        assert_eq!(names(&commands), Vec::<String>::new());
        assert_eq!(mark(&commands).1, 0);
    }

    #[test]
    fn a_tree_without_messages_names_nothing() {
        let commands = tree(0, vec![root(&[1]), literal("spawn", &[])]);

        assert_eq!(names(&commands), Vec::<String>::new());
    }

    /// A chain of 10 000 literals to one message: every node visited once.
    #[test]
    fn a_deep_chain_is_one_pass() {
        let depth: u32 = 10_000;
        let mut entries = vec![root(&[1])];
        for at in 1..=depth {
            entries.push(literal(&format!("l{at}"), &[at + 1]));
        }
        entries.push(message());
        let commands = tree(0, entries);

        let (found, visited) = mark(&commands);

        assert!(found.contains("l1"));
        assert_eq!(visited, commands.entries.len());
    }

    /// 1000 commands sharing 100 arguments that all lead to one message:
    /// each node is visited once, never once per path.
    #[test]
    fn shared_arguments_are_visited_once() {
        let count: u32 = 1_000;
        let shared: u32 = 100;
        let first_shared = 1 + count;
        let message_at = first_shared + shared;
        let shared_children: Vec<u32> = (first_shared..message_at).collect();
        let mut entries = vec![root(&(1..=count).collect::<Vec<_>>())];
        for at in 1..=count {
            entries.push(literal(&format!("c{at}"), &shared_children));
        }
        for _ in 0..shared {
            entries.push(targets(&[message_at]));
        }
        entries.push(message());
        let commands = tree(0, entries);

        let (found, visited) = mark(&commands);

        assert_eq!(found.0.len(), 1_000);
        assert_eq!(visited, commands.entries.len());
    }

    #[test]
    fn a_name_is_matched_exactly() {
        let found = MessageCommands::of(&vanilla());

        assert!(found.contains("me"));
        assert!(!found.contains("Me"));
        assert!(!found.contains("/me"));
        assert!(!found.contains("spawn"));
    }
}
