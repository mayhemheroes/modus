// Copyright 2021 Sergey Mechtaev

// This file is part of Modus.

// Modus is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Modus is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Modus.  If not, see <https://www.gnu.org/licenses/>.

use std::{
    collections::{HashMap, HashSet},
    fmt::{Debug, Display},
    hash::Hash,
};

use crate::unification::{compose_extend, compose_no_extend, Rename, Substitution};
use crate::{
    logic::{self, Signature},
    unification::Substitute,
    wellformed,
};
use logic::{Atom, Clause, Literal, Term};

pub trait Variable<C, V>: Rename<C, V> {
    fn aux() -> Self;
}

type RuleId = usize;
type TreeLevel = usize;
pub(crate) type Goal<C, V> = Vec<Literal<C, V>>;

/// A clause is either a rule, or a query
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ClauseId {
    Rule(RuleId),
    Query,
}

/// A literal origin can be uniquely identified through its source clause and its index in the clause body
#[derive(Clone, PartialEq, Debug)]
pub struct LiteralOrigin {
    clause: ClauseId,
    body_index: usize,
}

/// Literal identifier relative to the goal
type LiteralGoalId = usize;

fn literal_by_id<C, V>(
    rules: &Vec<Clause<C, V>>,
    query: &Goal<C, V>,
    id: LiteralOrigin,
) -> Literal<C, V>
where
    C: Clone,
    V: Clone,
{
    match id.clause {
        ClauseId::Query => query[id.body_index].clone(),
        ClauseId::Rule(rid) => rules[rid].body[id.body_index].clone(),
    }
}

/// literal, tree level at which it was introduced if any, where it came from
#[derive(Clone, PartialEq, Debug)]
struct LiteralWithHistory<C, V> {
    literal: Literal<C, V>,
    introduction: TreeLevel,
    origin: LiteralOrigin,
}
type GoalWithHistory<C, V> = Vec<LiteralWithHistory<C, V>>;

/// An SLD tree consists of
/// - a goal with its dependencies (at which level and from which part of body each literal was introduced)
/// - a level, which is incremented as tree grows
/// - a mapping from (selected literal in goal, applied rule) to (mgu after rule renaming, rule renaming, resolvent subtree)
#[derive(Clone, Debug)]
pub struct Tree<C, V> {
    goal: GoalWithHistory<C, V>,
    level: TreeLevel,
    resolvents:
        HashMap<(LiteralGoalId, ClauseId), (Substitution<C, V>, Substitution<C, V>, Tree<C, V>)>,
}

fn pretty_write<C: Display, V: Display>(
    tree: &Tree<C, V>,
    rules: &Vec<Clause<C, V>>,
    s: &mut String,
) {
    s.push('\n');
    s.push_str(&" ".repeat(tree.level));
    s.push_str(
        &tree
            .goal
            .iter()
            .map(|lit_history| format!("{}", lit_history.literal))
            .collect::<Vec<String>>()
            .join(","),
    );
    for (_, _, tree) in tree.resolvents.values() {
        pretty_write(tree, rules, s);
    }
}

impl<C: Display, V: Display> Tree<C, V> {
    pub fn pretty_print(&self, rules: &Vec<Clause<C, V>>) {
        let mut s = self
            .goal
            .iter()
            .map(|lit_history| format!("{}", lit_history.literal))
            .collect::<Vec<String>>()
            .join(",");
        for (_, _, tree) in self.resolvents.values() {
            pretty_write(tree, rules, &mut s);
        }
        println!("{}", s);
    }
}

/// A proof tree consist of
/// - a clause
/// - a valuation for this clause
/// - proofs for parts of the clause body
#[derive(Clone, Debug)]
pub struct Proof<C, V> {
    pub clause: ClauseId,
    pub valuation: Substitution<C, V>,
    pub children: Vec<Proof<C, V>>,
}

impl<C: Clone, V: Eq + Hash + Clone> Substitute<C, V> for GoalWithHistory<C, V> {
    type Output = GoalWithHistory<C, V>;
    fn substitute(&self, s: &Substitution<C, V>) -> Self::Output {
        self.iter()
            .map(
                |LiteralWithHistory {
                     literal,
                     introduction,
                     origin,
                 }| LiteralWithHistory {
                    literal: literal.substitute(s),
                    introduction: introduction.clone(),
                    origin: origin.clone(),
                },
            )
            .collect()
    }
}

pub fn sld<C, V>(
    rules: &Vec<Clause<C, V>>,
    goal: &Goal<C, V>,
    maxdepth: TreeLevel,
) -> Option<Tree<C, V>>
where
    C: Clone + PartialEq + Debug,
    V: Clone + Eq + Hash + Variable<C, V> + Debug,
{
    /// select leftmost literal with compatible groundness
    fn select<C, V>(
        goal: &GoalWithHistory<C, V>,
        grounded: &HashMap<Signature, Vec<bool>>,
    ) -> Option<(LiteralGoalId, LiteralWithHistory<C, V>)>
    where
        C: Clone,
        V: Clone + Eq + Hash,
    {
        let mut selected: Option<(LiteralGoalId, LiteralWithHistory<C, V>)> = None;
        'outer: for (id, lit) in goal.iter().enumerate() {
            let LiteralWithHistory {
                literal,
                introduction: _,
                origin: _,
            } = lit;
            let lit_grounded = grounded.get(&literal.signature()).unwrap();
            let mut matching = true;
            literal
                .args
                .iter()
                .enumerate()
                .for_each(|(id, t)| match (t, lit_grounded[id]) {
                    (Term::Variable(_), false) => matching = false,
                    _ => (),
                });
            if matching {
                selected = Some((id, lit.clone()));
                break 'outer;
            }
        }
        selected
    }

    fn resolve<C, V>(
        lid: LiteralGoalId,
        rid: RuleId,
        goal: &GoalWithHistory<C, V>,
        mgu: &Substitution<C, V>,
        rule: &Clause<C, V>,
        level: TreeLevel,
    ) -> GoalWithHistory<C, V>
    where
        C: Clone + PartialEq + Debug,
        V: Clone + Eq + Hash + Variable<C, V> + Debug,
    {
        let mut g: GoalWithHistory<C, V> = goal.clone();
        g.remove(lid);
        g.extend(
            rule.body
                .iter()
                .enumerate()
                .map(|(id, l)| {
                    let origin = LiteralOrigin {
                        clause: ClauseId::Rule(rid),
                        body_index: id,
                    };
                    LiteralWithHistory {
                        literal: l.clone(),
                        introduction: level,
                        origin,
                    }
                })
                .collect::<GoalWithHistory<C, V>>(),
        );
        g.substitute(mgu)
    }

    fn inner<C, V>(
        rules: &Vec<Clause<C, V>>,
        goal: &GoalWithHistory<C, V>,
        maxdepth: TreeLevel,
        level: TreeLevel,
        grounded: &HashMap<Signature, Vec<bool>>,
    ) -> Option<Tree<C, V>>
    where
        C: Clone + PartialEq + Debug,
        V: Clone + Eq + Hash + Variable<C, V> + Debug,
    {
        if goal.is_empty() {
            Some(Tree {
                goal: goal.clone(),
                level,
                resolvents: HashMap::new(),
            })
        } else if level >= maxdepth {
            None
        } else {
            let selected = select(goal, grounded);
            if selected.is_none() {
                return None;
            }
            let (lid, l) = selected.unwrap();
            let resolvents: HashMap<
                (LiteralGoalId, ClauseId),
                (Substitution<C, V>, Substitution<C, V>, Tree<C, V>),
            > = rules
                .iter()
                .enumerate()
                .filter(|(_, c)| c.head.signature() == l.literal.signature())
                .map(|(rid, c)| (rid, c.rename()))
                .filter_map(|(rid, (c, renaming))| {
                    c.head.unify(&l.literal).and_then(|mgu| {
                        Some((
                            rid,
                            mgu.clone(),
                            renaming,
                            resolve(lid, rid, &goal, &mgu, &c, level + 1),
                        ))
                    })
                })
                .filter_map(|(rid, mgu, renaming, resolvent)| {
                    inner(rules, &resolvent, maxdepth, level + 1, grounded)
                        .and_then(|tree| Some(((lid, ClauseId::Rule(rid)), (mgu, renaming, tree))))
                })
                .collect();
            if resolvents.is_empty() {
                None
            } else {
                Some(Tree {
                    goal: goal.clone(),
                    level,
                    resolvents,
                })
            }
        }
    }

    let grounded = wellformed::check_grounded_variables(rules).unwrap();

    let goal_with_history = goal
        .iter()
        .enumerate()
        .map(|(id, l)| {
            let origin = LiteralOrigin {
                clause: ClauseId::Query,
                body_index: id,
            };
            LiteralWithHistory {
                literal: l.clone(),
                introduction: 0,
                origin,
            }
        })
        .collect();
    inner(rules, &goal_with_history, maxdepth, 0, &grounded)
}

pub fn solutions<C, V>(tree: &Tree<C, V>) -> HashSet<Goal<C, V>>
where
    C: Clone + Eq + Hash + Debug + Display,
    V: Clone + Eq + Hash + Variable<C, V> + Debug + Display,
{
    fn inner<C, V>(tree: &Tree<C, V>) -> Vec<Substitution<C, V>>
    where
        C: Clone,
        V: Clone + Eq + Hash + Debug,
    {
        if tree.goal.is_empty() {
            let s = Substitution::<C, V>::new();
            return vec![s];
        }
        tree.resolvents
            .iter()
            .map(|(_, (mgu, _, subtree))| (mgu, inner(subtree)))
            .map(|(mgu, sub)| {
                sub.iter()
                    .map(|s| compose_extend(mgu, s))
                    .collect::<Vec<Substitution<C, V>>>()
            })
            .flatten()
            .collect()
    }
    inner(tree)
        .iter()
        .map(|s| {
            tree.goal
                .iter()
                .map(
                    |LiteralWithHistory {
                         literal,
                         introduction: _,
                         origin: _,
                     }| literal.substitute(s),
                )
                .collect()
        })
        .collect()
}

#[derive(Clone)]
struct PathNode<C, V> {
    resolvent: GoalWithHistory<C, V>,
    applied: ClauseId,
    selected: LiteralGoalId,
    renaming: Substitution<C, V>,
}

// sequence of nodes and global mgu
type Path<C, V> = (Vec<PathNode<C, V>>, Substitution<C, V>);

pub fn proofs<C, V>(
    tree: &Tree<C, V>,
    rules: &Vec<Clause<C, V>>,
    goal: &Goal<C, V>,
) -> Vec<Proof<C, V>>
where
    C: Clone + Eq + Hash + Debug,
    V: Clone + Eq + Hash + Variable<C, V> + Debug,
{
    fn flatten_compose<C, V>(
        lid: &LiteralGoalId,
        cid: &ClauseId,
        mgu: &Substitution<C, V>,
        renaming: &Substitution<C, V>,
        tree: &Tree<C, V>,
    ) -> Vec<Path<C, V>>
    where
        C: Clone + Eq + Hash,
        V: Clone + Eq + Hash,
    {
        if tree.goal.is_empty() {
            return vec![(
                vec![PathNode {
                    resolvent: tree.goal.clone(),
                    applied: cid.clone(),
                    selected: lid.clone(),
                    renaming: renaming.clone(),
                }],
                mgu.clone(),
            )];
        }
        tree.resolvents
            .iter()
            .map(|((sub_lid, sub_cid), (sub_mgu, sub_renaming, sub_tree))| {
                flatten_compose(sub_lid, sub_cid, sub_mgu, sub_renaming, sub_tree)
                    .iter()
                    .map(|(sub_path, sub_val)| {
                        let mut nodes = vec![PathNode {
                            resolvent: tree.goal.clone(),
                            applied: cid.clone(),
                            selected: lid.clone(),
                            renaming: renaming.clone(),
                        }];
                        let val = compose_extend(mgu, sub_val);
                        nodes.extend(sub_path.clone());
                        (nodes, val)
                    })
                    .collect::<Vec<Path<C, V>>>()
            })
            .flatten()
            .collect()
    }
    // reconstruct proof for a given tree level
    fn proof_for_level<C, V>(
        path: &Vec<PathNode<C, V>>,
        mgu: &Substitution<C, V>,
        rules: &Vec<Clause<C, V>>,
        level: TreeLevel,
    ) -> Proof<C, V>
    where
        C: Clone + Eq + Hash,
        V: Clone + Eq + Hash,
    {
        let mut sublevels_map: HashMap<usize, TreeLevel> = HashMap::new();
        for l in 0..path.len() {
            if !path[l].resolvent.is_empty() {
                let resolved_child = path[l].resolvent[path[l + 1].selected].clone();
                if resolved_child.introduction == level {
                    sublevels_map.insert(resolved_child.origin.body_index, l + 1);
                }
            }
        }
        let children_length = sublevels_map.len();
        match path[level].applied {
            ClauseId::Query => assert_eq!(children_length, path[0].resolvent.len()),
            ClauseId::Rule(rid) => assert_eq!(children_length, rules[rid].body.len()),
        };

        let mut sublevels = Vec::<TreeLevel>::with_capacity(sublevels_map.len());
        for k in sublevels_map.keys() {
            assert!(*k < children_length);
        }
        for i in 0..children_length {
            sublevels.push(*sublevels_map.get(&i).unwrap());
        }
        Proof {
            clause: path[level].applied.clone(),
            valuation: compose_no_extend(&path[level].renaming, mgu),
            children: sublevels
                .iter()
                .map(|l| proof_for_level(path, mgu, rules, *l))
                .collect(),
        }
    }
    // assume lid of root is 0, as if it came from a clause "true :- goal" for query "true", but this is not used anyway
    let goal_vars = goal
        .iter()
        .map(|l| l.variables())
        .reduce(|mut l, r| {
            l.extend(r);
            l
        })
        .unwrap_or_default();
    let goal_id_reaming: Substitution<C, V> = goal_vars
        .iter()
        .map(|v| (v.clone(), Term::Variable(v.clone())))
        .collect();
    let paths = flatten_compose(
        &0,
        &ClauseId::Query,
        &Substitution::new(),
        &goal_id_reaming,
        tree,
    );
    let all_proofs: Vec<Proof<C, V>> = paths
        .iter()
        .map(|(path, mgu)| proof_for_level(path, mgu, rules, 0))
        .collect();

    //TODO: instead, I should find optimal proofs
    let mut computed: HashSet<Goal<C, V>> = HashSet::new();
    let mut proofs: Vec<Proof<C, V>> = Vec::new();
    for p in all_proofs {
        let solution: Goal<C, V> = goal.substitute(&p.valuation);
        if !computed.contains(&solution) {
            computed.insert(solution.clone());
            proofs.push(p)
        }
    }
    println!("proofs: {}", proofs.len());
    proofs
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicU32, Ordering},
    };

    use super::*;

    static AVAILABLE_INDEX: AtomicU32 = AtomicU32::new(0);

    /// Assume that underscore is not used in normal variables
    impl Rename<logic::Atom, logic::toy::Variable> for logic::toy::Variable {
        type Output = logic::toy::Variable;
        fn rename(
            &self,
        ) -> (
            Self::Output,
            Substitution<logic::Atom, logic::toy::Variable>,
        ) {
            let index = AVAILABLE_INDEX.fetch_add(1, Ordering::SeqCst);
            let prefix = self.split('_').next().unwrap();
            let renamed = format!("{}_{}", prefix, index);
            let mut s = HashMap::<
                logic::toy::Variable,
                logic::Term<logic::Atom, logic::toy::Variable>,
            >::new();
            s.insert(self.clone(), logic::Term::Variable(renamed.clone()));
            (renamed, s)
        }
    }

    impl Variable<logic::Atom, logic::toy::Variable> for logic::toy::Variable {
        fn aux() -> logic::toy::Variable {
            let index = AVAILABLE_INDEX.fetch_add(1, Ordering::SeqCst);
            format!("Aux{}", index)
        }
    }

    #[test]
    fn simple_solving() {
        let goal: Goal<logic::Atom, logic::toy::Variable> = vec!["a(X)".parse().unwrap()];
        let clauses: Vec<logic::toy::Clause> = vec![
            "a(X) :- b(X)".parse().unwrap(),
            logic::toy::Clause {
                head: "b(c)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "b(d)".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 10);
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 2);
        assert!(solutions.contains(&vec!["a(c)".parse::<logic::toy::Literal>().unwrap()]));
        assert!(solutions.contains(&vec!["a(d)".parse::<logic::toy::Literal>().unwrap()]));
    }

    #[test]
    fn simple_nongrounded() {
        let goal: Goal<logic::Atom, logic::toy::Variable> = vec!["a(b)".parse().unwrap()];
        let clauses: Vec<logic::toy::Clause> = vec![logic::toy::Clause {
            head: "a(X)".parse().unwrap(),
            body: vec![],
        }];
        let result = sld(&clauses, &goal, 10);
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 1);
        assert!(solutions.contains(&vec!["a(b)".parse::<logic::toy::Literal>().unwrap()]));
    }

    #[test]
    fn simple_nongrounded_invalid() {
        let goal: Goal<logic::Atom, logic::toy::Variable> = vec!["a(X)".parse().unwrap()];
        let clauses: Vec<logic::toy::Clause> = vec![logic::toy::Clause {
            head: "a(X)".parse().unwrap(),
            body: vec![],
        }];
        let result = sld(&clauses, &goal, 10);
        assert!(result.is_none());
    }

    #[test]
    fn complex_goal() {
        let goal: Goal<logic::Atom, logic::toy::Variable> =
            vec!["a(X)".parse().unwrap(), "b(X)".parse().unwrap()];
        let clauses: Vec<logic::toy::Clause> = vec![
            logic::toy::Clause {
                head: "a(t)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "a(f)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "b(g)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "b(t)".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 10);
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 1);
        assert!(solutions.contains(&vec!["a(t)".parse().unwrap(), "b(t)".parse().unwrap()]));
    }

    #[test]
    fn solving_with_binary_relations() {
        let goal: Goal<logic::Atom, logic::toy::Variable> = vec!["a(X)".parse().unwrap()];
        let clauses: Vec<logic::toy::Clause> = vec![
            "a(X) :- b(X, Y), c(Y)".parse().unwrap(),
            logic::toy::Clause {
                head: "b(t, f)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "b(f, t)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "b(g, t)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "c(t)".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 10);
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 2);
        assert!(solutions.contains(&vec!["a(f)".parse().unwrap()]));
        assert!(solutions.contains(&vec!["a(g)".parse().unwrap()]));
    }

    #[test]
    fn simple_recursion() {
        let goal: Goal<logic::Atom, logic::toy::Variable> = vec!["reach(a, X)".parse().unwrap()];
        let clauses: Vec<logic::toy::Clause> = vec![
            "reach(X, Y) :- reach(X, Z), arc(Z, Y)".parse().unwrap(),
            "reach(X, Y) :- arc(X, Y)".parse().unwrap(),
            logic::toy::Clause {
                head: "arc(a, b)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "arc(b, c)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "arc(c, d)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "arc(d, e)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "arc(f, e)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "arc(g, f)".parse().unwrap(),
                body: vec![],
            },
            logic::toy::Clause {
                head: "arc(g, a)".parse().unwrap(),
                body: vec![],
            },
        ];
        let result = sld(&clauses, &goal, 15);
        assert!(result.is_some());
        let solutions = solutions(&result.unwrap());
        assert_eq!(solutions.len(), 4);
        assert!(solutions.contains(&vec!["reach(a, b)".parse().unwrap()]));
        assert!(solutions.contains(&vec!["reach(a, c)".parse().unwrap()]));
        assert!(solutions.contains(&vec!["reach(a, d)".parse().unwrap()]));
        assert!(solutions.contains(&vec!["reach(a, e)".parse().unwrap()]));
    }
}
