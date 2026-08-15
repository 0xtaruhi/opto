// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Fallible incremental-SAT adapter for the formal encoders.
//!
//! The encoders construct literals and clauses through infallible local
//! primitives so that Boolean expression construction stays compact. `CaDiCaL`
//! can nevertheless reject a clause or exceed the `RustSAT` variable space. The
//! adapter retains the first construction error and returns it at the next
//! solve boundary; no proof result can therefore be published from a partial
//! formula. Assumptions replace the previous query's assumptions and remain
//! active until explicitly replaced, matching the incremental contract used by
//! the formal encoders.

use anyhow::anyhow;
use rustsat::solvers::{Solve, SolveIncremental, SolverResult};
pub(crate) use rustsat::types::Lit;
use rustsat::types::{TernaryVal, Var};
use rustsat_cadical::CaDiCaL;

#[derive(Debug)]
pub(crate) struct SatSolver {
    backend: CaDiCaL<'static, 'static>,
    assumptions: Vec<Lit>,
    next_variable: u32,
    construction_error: Option<anyhow::Error>,
}

impl SatSolver {
    pub(crate) fn new() -> Self {
        Self {
            backend: CaDiCaL::default(),
            assumptions: Vec::new(),
            next_variable: 0,
            construction_error: None,
        }
    }

    pub(crate) fn new_lit(&mut self) -> Lit {
        if self.next_variable <= Var::MAX_IDX {
            let variable = Var::new(self.next_variable);
            self.next_variable += 1;
            return variable.pos_lit();
        }
        self.remember_error(anyhow!(
            "SAT variable capacity exceeds RustSAT's maximum index {}",
            Var::MAX_IDX
        ));
        Var::new(0).pos_lit()
    }

    pub(crate) fn add_clause(&mut self, literals: &[Lit]) {
        if self.construction_error.is_some() {
            return;
        }
        if let Err(source) = self.backend.add_clause_ref(literals) {
            self.remember_error(source);
        }
    }

    pub(crate) fn assume(&mut self, assumptions: &[Lit]) {
        self.assumptions.clear();
        self.assumptions.extend_from_slice(assumptions);
    }

    pub(crate) fn solve(&mut self) -> anyhow::Result<bool> {
        if let Some(source) = &self.construction_error {
            return Err(anyhow!("SAT formula construction failed: {source:#}"));
        }
        let result = if self.assumptions.is_empty() {
            self.backend.solve()?
        } else {
            self.backend.solve_assumps(&self.assumptions)?
        };
        match result {
            SolverResult::Sat => Ok(true),
            SolverResult::Unsat => Ok(false),
            SolverResult::Interrupted => Err(anyhow!("CaDiCaL solving was interrupted")),
        }
    }

    pub(crate) fn literal_value(&self, literal: Lit) -> anyhow::Result<Option<bool>> {
        match self.backend.lit_val(literal)? {
            TernaryVal::True => Ok(Some(true)),
            TernaryVal::False => Ok(Some(false)),
            TernaryVal::DontCare => Ok(None),
        }
    }

    fn remember_error(&mut self, source: anyhow::Error) {
        if self.construction_error.is_none() {
            self.construction_error = Some(source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SatSolver;
    use anyhow::anyhow;

    #[test]
    fn incremental_assumptions_replace_the_previous_query() {
        let mut solver = SatSolver::new();
        let value = solver.new_lit();

        solver.assume(&[value]);
        assert!(solver.solve().expect("positive assumption is satisfiable"));
        assert_eq!(
            solver
                .literal_value(value)
                .expect("satisfying assignment is readable"),
            Some(true)
        );

        solver.assume(&[!value]);
        assert!(solver.solve().expect("negative assumption is satisfiable"));
        assert_eq!(
            solver
                .literal_value(value)
                .expect("replacement assignment is readable"),
            Some(false)
        );

        solver.add_clause(&[value]);
        assert!(!solver.solve().expect("conflicting query is unsatisfiable"));
        solver.assume(&[]);
        assert!(solver.solve().expect("cleared assumptions are satisfiable"));
    }

    #[test]
    fn construction_failure_permanently_poisons_the_solver() {
        let mut solver = SatSolver::new();
        solver.remember_error(anyhow!("synthetic construction failure"));

        for _ in 0..2 {
            assert_eq!(
                solver
                    .solve()
                    .expect_err("a partial formula must never be solved")
                    .to_string(),
                "SAT formula construction failed: synthetic construction failure"
            );
        }
    }
}
