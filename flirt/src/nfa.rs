/// ## purpose
/// to match multiple byte patterns against a byte slice in parallel.
/// we should get all valid matches at the end.
/// does not have to support scanning across the byte slice, only anchored at the start.
/// need support for single character wild cards (`.`).
/// all patterns are the same length
/// (if this needs to change, maybe pad shorter patterns with wildcards).
///
/// ## design:
/// we'll build an NFA with symbols for:
///   - all valid byte values (0-255), and
///   - a wildcard
///
/// a transition table will have 257 columns, one for each of the above symbols, including wildcard.
/// the transition table has a row for each state.
/// entries in each column indicate valid transitions.
/// an entry of `0` indicates "invalid".
/// if a row contains only invalid entries, then its a "terminal state".
/// there's a list associated with each row of "alive" patterns;
/// for a terminal state, these are the patterns that matched.
///
///
/// ### example (no wildcards)
///
/// input patterns:
///
///   p0: A B C D
///   p1: A D C B
///
/// transition table:
///
/// ```ignore
///       A B C D
///   0 | 1        alive: p0, p1
///   1 |   2   4  alive: p0, p1
///   2 |     3    alive: p0
///   3 |          terminal, alive: p0
///   4 |     5    alive: p1
///   5 |          terminal, alive: p1
/// ```
use std::collections::VecDeque;

use nom::IResult;
use nom::multi::many1;
use nom::bytes::complete::tag;
use nom::bytes::complete::take_while_m_n;
use nom::branch::alt;
use nom::combinator::map;
use nom::combinator::map_res;
use bitvec::prelude::*;
use log::Level::Warn;


// u16 because we need 257 possible values, all unsigned.
#[derive(Copy, Clone)]
pub struct Symbol(u16);

// impl note: value 256 is WILDCARD.
pub const WILDCARD: Symbol = Symbol(0x100);

// byte values map directly into their Symbol indices.
impl std::convert::From<u8> for Symbol {
    fn from(v: u8) -> Self {Symbol(v as u16)}
}

// convert to usize so we can index into `State.transitions`.
impl std::convert::Into<usize> for Symbol {
    fn into(self) -> usize {self.0 as usize}
}

impl std::cmp::PartialEq for Symbol {
    fn eq(&self, other: &Symbol) -> bool {
        self.0 == other.0
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.0 == WILDCARD.0 {
            write!(f, "..")
        } else {
            write!(f, "{:02x}", self.0)
        }
    }
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

// a pattern is just a sequence of symbols.
pub struct Pattern(Vec<Symbol>);

impl std::fmt::Display for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|s| format!("{}", s)).collect();
        write!(f, "{}", parts.join(""))
    }
}

fn is_hex_digit(c: char) -> bool {
  c.is_digit(16)
}

fn from_hex(input: &str) -> Result<u8, std::num::ParseIntError> {
  u8::from_str_radix(input, 16)
}

/// parse a single hex byte, like `AB`
fn hex(input: &str) -> IResult<&str, u8> {
    map_res(
        take_while_m_n(2, 2, is_hex_digit),
        from_hex
    )(input)
}

/// parse a single byte signature element, which is either a hex byte or a wildcard.
fn sig_element(input: &str) -> IResult<&str, Symbol> {
    alt((
        map(hex, |v| Symbol::from(v)),
        map(tag(".."), |_| WILDCARD),
    ))(input)
}

/// parse byte signature elements, hex or wildcard.
fn byte_signature(input: &str) -> IResult<&str, Pattern> {
    let (input, elems) = many1(sig_element)(input)?;
    Ok((input, Pattern(elems)))
}

/// parse a pattern from a string like `AABB..DD`.
impl std::convert::From<&str> for Pattern {
    fn from(v: &str) -> Self {
        byte_signature(v).expect("failed to parse pattern").1
    }
}

// u32 because we need a fairly large range: these transition tables can have many rows.
// positive values are indexes to a transition table row.
// the zero value indicates an invalid transition, and rows are initially filled with this.
#[derive(Copy, Clone)]
struct Transition(u32);

impl Default for Transition {
    fn default() -> Self {Transition(0)}
}

impl std::fmt::Display for Transition {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl std::fmt::Debug for Transition {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

// convert to usize so we can index into `StateTable.states`.
impl std::convert::Into<usize> for Transition {
    fn into(self) -> usize {self.0 as usize}
}

impl Transition {
    fn is_valid(&self) -> bool {
        self.0 != 0
    }
}

struct State {
    // 257 to cover the max range of a symbol.
    transitions: [Transition; 257],
    // indices with the bit set indicates the corresponding index in `NFA.patterns` is alive.
    alive: BitVec,
}

impl State {
    fn new(capacity: u32) -> State {
        State {
            transitions: [Default::default(); 257],
            alive: bitvec![0; capacity as usize],
        }
    }

    // a state is terminal if it has no valid transitions from it.
    fn is_terminal(&self) -> bool {
        self.transitions
            .iter()
            .find(|&transition| transition.is_valid())
            .is_none()
    }
}

struct StateTable {
    // total number of patterns in NFA.
    //
    // we have to know this before we start constructing the table,
    // that's why we use the Builder pattern.
    capacity: u32,
    states: Vec<State>,
}

impl StateTable {
    /// add a new state to the end of the existing table.
    /// return the index of the state as a `Transition`.
    fn add_state(&mut self) -> Transition {
        let index = self.states.len() as u32;  // TODO: danger
        self.states.push(State::new(self.capacity));
        Transition(index)
    }
}

pub struct NFA {
    table: StateTable,
    patterns: Vec<Pattern>,
}

impl NFA {
    pub fn r#match(&self, buf: &[u8]) -> Vec<&Pattern> {
        // TODO
        vec![]
    }

    pub fn new() -> NFABuilder {
        NFABuilder {
            patterns: vec![],
        }
    }
}


// output looks something like:
//
//
//     patterns:
//       - pat0: aabbccdd
//       - pat1: aabbcccc
//
//     transition table:
//
//       aa  bb  cc  dd  ..
//       0:  1                   alive: pat0 pat1
//       1:      2               alive: pat0 pat1
//       2:          3           alive: pat0 pat1
//       3:          5   4       alive: pat0 pat1
//       4:                      matches: pat0
//       5:                      matches: pat1
// ````
impl std::fmt::Debug for NFA {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "patterns:").unwrap();
        for (i, pattern) in self.patterns.iter().enumerate() {
            writeln!(f, "  - pat{}: {}", i, pattern).unwrap();
        }
        writeln!(f, "").unwrap();

        // TODO: compute these dynamically
        let symbols = [Symbol(0xAA), Symbol(0xBB), Symbol(0xCC), Symbol(0xDD), WILDCARD];
        writeln!(f, "transition table:").unwrap();
        write!(f, "     ");
        for symbol in symbols.iter() {
            write!(f, " {} ", symbol).unwrap();
        }
        writeln!(f, "").unwrap();

        for (i, state) in self.table.states.iter().enumerate() {
            write!(f, "  {:>2}:", i).unwrap();

            for &symbol in symbols.iter() {
                let index: usize = symbol.into();
                let transition = state.transitions[index];
                if transition.is_valid() {
                    write!(f, " {:>2} ", transition.0).unwrap();
                } else {
                    write!(f, "    ").unwrap();
                }
            }

            if state.alive.iter().find(|&b| b).is_some() {
                if state.is_terminal() {
                    write!(f, "  matches:").unwrap();
                } else {
                    write!(f, "  alive:").unwrap();
                }
                for (i, is_alive) in state.alive.iter().enumerate() {
                    if is_alive {
                        write!(f, " pat{}", i).unwrap();
                    }
                }
            }
            writeln!(f, "").unwrap();
        }

        write!(f, "")  // OK
    }
}

pub struct NFABuilder {
    patterns: Vec<Pattern>
}

impl NFABuilder {
    pub fn add_pattern(&mut self, pattern: Pattern) {
        self.patterns.push(pattern)
    }

    pub fn build(self) -> NFA {
        let mut nfa = NFA {
            table: StateTable {
                capacity: self.patterns.len() as u32,  // TODO: danger.
                states: vec![],
            },
            patterns: self.patterns,
        };

        let start_state = nfa.table.add_state();

        #[derive(Debug)]
        struct Step<'a> {
            /// the index to the state that we need to add the next symbol.
            state_pointer: Transition,
            /// the index of the pattern we're currently processing.
            pattern_index: usize,
            /// the remaining symbols that need to be added.
            symbols: &'a[Symbol],
        };

        let mut q: VecDeque<Step> = VecDeque::new();
        for (pattern_index, pattern) in nfa.patterns.iter().enumerate() {
            q.push_back(Step {
                state_pointer: start_state,
                pattern_index: pattern_index,
                symbols: &pattern.0[..]
            })
        }

        while let Some(step) = q.pop_front() {
            println!("state:\n{:?}", nfa);
            println!("step: {:?}", step);

            // I'm not quite sure how to cast this correctly.
            // maybe we need the Index type???
            let index: usize = step.state_pointer.into();
            let mut state = &mut nfa.table.states[index];

            // mark the current pattern as "alive" at the given state.
            state.alive.set(step.pattern_index, true);

            if step.symbols.len() == 0 {
                // this is terminal.
                continue
            }

            // fetch the symbol to insert, like AA
            let symbol = step.symbols[0];

            // fetch the cell from the state table,
            // this will either:
            //
            //   1. be invalid, which means we have to set it, or
            //   2. be already set to an existing transition
            //
            // if the value is unset, then we need to allocate a new row,
            // and point this cell towards it.
            let index: usize = symbol.into();
            let transition: Transition = state.transitions[index];

            if step.symbols[0] == WILDCARD {
                // wildcard

            } else {
                // byte value
                let next_state = if transition.is_valid() {
                    // there is already a pointer to a state
                    transition
                } else {
                    // need to alloc a new state
                    let next_state = nfa.table.add_state();

                    let index: usize = step.state_pointer.into();
                    let mut state = &mut nfa.table.states[index];

                    let index: usize = step.symbols[0].into();
                    state.transitions[index] = next_state;

                    next_state
                };

                q.push_back(Step {
                    state_pointer: next_state,
                    pattern_index: step.pattern_index,
                    symbols: &step.symbols[1..]
                })
            }
        }

        println!("final state:\n{:?}", nfa);

        nfa
    }


}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_build() {
        let nfa = NFA::new().build();
    }

    // patterns:
    //   - pat0: aabbccdd
    //
    // transition table:
    //
    //  aa  bb  cc  dd  ..
    //  0:  1                   alive: pat0
    //  1:      2               alive: pat0
    //  2:          3           alive: pat0
    //  3:              4       alive: pat0
    //  4:                      matches: pat0
    #[test]
    fn test_add_one_pattern() {
        let mut b = NFA::new();
        b.add_pattern(Pattern::from("AABBCCDD"));

        let nfa = b.build();
    }

    // patterns:
    //   - pat0: aabbccdd
    //   - pat1: aabbcccc
    //
    // transition table:
    //
    //  aa  bb  cc  dd  ..
    //  0:  1                   alive: pat0 pat1
    //  1:      2               alive: pat0 pat1
    //  2:          3           alive: pat0 pat1
    //  3:          5   4       alive: pat0 pat1
    //  4:                      matches: pat0
    //  5:                      matches: pat1
    #[test]
    fn test_add_two_patterns() {
        let mut b = NFA::new();
        b.add_pattern(Pattern::from("AABBCCDD"));
        b.add_pattern(Pattern::from("AABBCCCC"));

        let nfa = b.build();
        assert!(false);
    }
}
