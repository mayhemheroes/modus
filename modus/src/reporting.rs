// Modus, a language for building container images
// Copyright (C) 2022 University College London

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.

// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use std::{fmt::Display, io::Write};

use serde::Serialize;

use modus_lib::{
    imagegen::BuildPlan,
    logic::{IRTerm, Literal},
};

pub type BuildResult = Vec<Image>;

#[derive(Serialize, Debug, Clone)]
pub struct ConstantLiteral {
    pub predicate: String,
    pub args: Vec<String>,
}

impl ConstantLiteral {
    pub fn from_literal(lit: Literal) -> Self {
        Self {
            predicate: lit.predicate.0,
            args: lit
                .args
                .into_iter()
                .map(|x| match x {
                    IRTerm::Constant(x) => x,
                    _ => panic!("Expected constant"),
                })
                .collect::<Vec<_>>(),
        }
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct Image {
    #[serde(flatten)]
    pub source_literal: ConstantLiteral,
    pub digest: String,
}

pub fn write_build_result<F: Write, P: Display>(
    mut json_out: F,
    json_out_name: P,
    build_plan: &BuildPlan,
    image_ids: &[String],
) -> Result<(), String> {
    debug_assert_eq!(build_plan.outputs.len(), image_ids.len());
    debug_assert!(build_plan
        .outputs
        .iter()
        .all(|x| x.source_literal.is_some()));

    let res = build_plan
        .outputs
        .iter()
        .zip(image_ids)
        .map(|(o, i)| Image {
            source_literal: ConstantLiteral::from_literal(
                o.source_literal.as_ref().unwrap().clone(),
            ),
            digest: i.clone(),
        })
        .collect::<Vec<_>>();

    json_out
        .write_all(
            &serde_json::to_vec_pretty(&res).map_err(|e| format!("Serialization error: {}", e))?,
        )
        .map_err(|e| format!("Error writing to {}: {}", json_out_name, e))?;

    Ok(())
}