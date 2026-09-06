// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::ImplementationRequest;
use super::compressor::{BitMatrix, CompressionSchedule};
use super::multiplier::ProductEncoding;
use super::{BitBackend, BitBlaster, ScalarBit};
use crate::OperatorKind;
use crate::planning::architecture::ArithmeticTerm;
use crate::planning::provider::{ImplementationProvider, ProviderRecipeId, StructuralEstimate};
use opto_ir::BitVal;
use opto_ir::word;

mod adders;
mod compare;
mod division;
mod shift;

const RIPPLE_CARRY: ProviderRecipeId = ProviderRecipeId::from_raw(0);
const BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(1);
const KOGGE_STONE: ProviderRecipeId = ProviderRecipeId::from_raw(2);
const HYBRID_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(3);
const AREA_HYBRID_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(4);
const INCREMENT: ProviderRecipeId = ProviderRecipeId::from_raw(5);
const DECREMENT: ProviderRecipeId = ProviderRecipeId::from_raw(6);
const CONSTANT_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(7);
const CONSTANT_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(8);
const SERIAL_CSA_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(9);
const SERIAL_CSA_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(10);
const BALANCED_CSA_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(11);
const BALANCED_CSA_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(12);
const WALLACE_CSA_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(13);
const WALLACE_CSA_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(14);
const DADDA_CSA_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(15);
const DADDA_CSA_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(16);
const SERIAL_RADIX4_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(17);
const SERIAL_RADIX4_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(18);
const BALANCED_RADIX4_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(19);
const BALANCED_RADIX4_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(20);
const WALLACE_RADIX4_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(21);
const WALLACE_RADIX4_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(22);
const DADDA_RADIX4_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(23);
const DADDA_RADIX4_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(24);
const SERIAL_ARRAY_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(25);
const SERIAL_ARRAY_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(26);
const BALANCED_ARRAY_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(27);
const BALANCED_ARRAY_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(28);
const WALLACE_ARRAY_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(29);
const WALLACE_ARRAY_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(30);
const DADDA_ARRAY_RIPPLE: ProviderRecipeId = ProviderRecipeId::from_raw(31);
const DADDA_ARRAY_BRENT_KUNG: ProviderRecipeId = ProviderRecipeId::from_raw(32);

const SERIAL_CSA_KOGGE_STONE: ProviderRecipeId = ProviderRecipeId::from_raw(33);
const BALANCED_CSA_KOGGE_STONE: ProviderRecipeId = ProviderRecipeId::from_raw(34);
const WALLACE_CSA_KOGGE_STONE: ProviderRecipeId = ProviderRecipeId::from_raw(35);
const DADDA_CSA_KOGGE_STONE: ProviderRecipeId = ProviderRecipeId::from_raw(36);

#[derive(Debug, Clone, Copy)]
enum CarryNetwork {
    Ripple,
    BrentKung,
    KoggeStone,
}

#[derive(Debug, Clone, Copy)]
struct RegionRecipe {
    schedule: CompressionSchedule,
    prefix: CarryNetwork,
    encoding: ProductEncoding,
}

struct ConstantProductGroup {
    multiplicand: word::ValueId,
    multiplicand_ty: word::WordType,
    coefficient: Vec<bool>,
}

#[derive(Debug)]
struct AddSubProvider;

impl ImplementationProvider for AddSubProvider {
    fn resource_name(&self) -> &'static str {
        "add-sub"
    }

    fn enumerate_recipes(
        &self,
        operator: crate::SemanticOperator,
        emit: &mut dyn FnMut(ProviderRecipeId),
    ) {
        match operator.kind() {
            OperatorKind::Add | OperatorKind::Subtract => {
                if operator.constant_input().is_some() {
                    emit(CONSTANT_RIPPLE);
                    if operator.width() >= 2 {
                        emit(CONSTANT_BRENT_KUNG);
                    }
                    return;
                }
                emit(RIPPLE_CARRY);
                if operator.width() >= 2 {
                    emit(BRENT_KUNG);
                }
                if operator.width() >= 3 {
                    emit(KOGGE_STONE);
                }
                if operator.width() >= 4 {
                    emit(HYBRID_BRENT_KUNG);
                }
                if operator.width() >= 5 {
                    emit(AREA_HYBRID_BRENT_KUNG);
                }
            }
            OperatorKind::Sum => {
                let recipes = if operator.variable_product_term_count() == 0 {
                    &[
                        SERIAL_CSA_RIPPLE,
                        SERIAL_CSA_BRENT_KUNG,
                        BALANCED_CSA_RIPPLE,
                        BALANCED_CSA_BRENT_KUNG,
                        WALLACE_CSA_RIPPLE,
                        WALLACE_CSA_BRENT_KUNG,
                        DADDA_CSA_RIPPLE,
                        DADDA_CSA_BRENT_KUNG,
                        SERIAL_CSA_KOGGE_STONE,
                        BALANCED_CSA_KOGGE_STONE,
                        WALLACE_CSA_KOGGE_STONE,
                        DADDA_CSA_KOGGE_STONE,
                    ][..]
                } else {
                    &[
                        SERIAL_RADIX4_RIPPLE,
                        SERIAL_RADIX4_BRENT_KUNG,
                        BALANCED_RADIX4_RIPPLE,
                        BALANCED_RADIX4_BRENT_KUNG,
                        WALLACE_RADIX4_RIPPLE,
                        WALLACE_RADIX4_BRENT_KUNG,
                        DADDA_RADIX4_RIPPLE,
                        DADDA_RADIX4_BRENT_KUNG,
                    ][..]
                };
                for (index, &recipe) in recipes.iter().enumerate() {
                    if (index < 8 && index % 2 == 0) || operator.width() >= 2 {
                        emit(recipe);
                    }
                }
                if operator.variable_product_term_count() != 0 {
                    for (index, &recipe) in [
                        SERIAL_ARRAY_RIPPLE,
                        SERIAL_ARRAY_BRENT_KUNG,
                        BALANCED_ARRAY_RIPPLE,
                        BALANCED_ARRAY_BRENT_KUNG,
                        WALLACE_ARRAY_RIPPLE,
                        WALLACE_ARRAY_BRENT_KUNG,
                        DADDA_ARRAY_RIPPLE,
                        DADDA_ARRAY_BRENT_KUNG,
                    ]
                    .iter()
                    .enumerate()
                    {
                        if index % 2 == 0 || operator.width() >= 2 {
                            emit(recipe);
                        }
                    }
                }
            }
            OperatorKind::Increment => emit(INCREMENT),
            OperatorKind::Decrement => emit(DECREMENT),
            OperatorKind::Multiply
            | OperatorKind::Divide
            | OperatorKind::Modulo
            | OperatorKind::DynamicExtract => {}
        }
    }

    fn recipe_name(&self, recipe: ProviderRecipeId) -> Option<&str> {
        match recipe {
            RIPPLE_CARRY => Some("ripple-carry"),
            BRENT_KUNG => Some("brent-kung"),
            KOGGE_STONE => Some("kogge-stone"),
            HYBRID_BRENT_KUNG => Some("hybrid-brent-kung-balanced"),
            AREA_HYBRID_BRENT_KUNG => Some("hybrid-brent-kung-area"),
            INCREMENT => Some("increment-ripple"),
            DECREMENT => Some("decrement-ripple"),
            CONSTANT_RIPPLE => Some("constant-ripple"),
            CONSTANT_BRENT_KUNG => Some("constant-brent-kung"),
            SERIAL_CSA_KOGGE_STONE => Some("serial-csa-kogge-stone"),
            BALANCED_CSA_KOGGE_STONE => Some("balanced-csa-kogge-stone"),
            WALLACE_CSA_KOGGE_STONE => Some("wallace-csa-kogge-stone"),
            DADDA_CSA_KOGGE_STONE => Some("dadda-csa-kogge-stone"),
            SERIAL_CSA_RIPPLE => Some("serial-csa-ripple"),
            SERIAL_CSA_BRENT_KUNG => Some("serial-csa-brent-kung"),
            BALANCED_CSA_RIPPLE => Some("balanced-csa-ripple"),
            BALANCED_CSA_BRENT_KUNG => Some("balanced-csa-brent-kung"),
            WALLACE_CSA_RIPPLE => Some("wallace-csa-ripple"),
            WALLACE_CSA_BRENT_KUNG => Some("wallace-csa-brent-kung"),
            DADDA_CSA_RIPPLE => Some("dadda-csa-ripple"),
            DADDA_CSA_BRENT_KUNG => Some("dadda-csa-brent-kung"),
            SERIAL_RADIX4_RIPPLE => Some("serial-radix4-ripple"),
            SERIAL_RADIX4_BRENT_KUNG => Some("serial-radix4-brent-kung"),
            BALANCED_RADIX4_RIPPLE => Some("balanced-radix4-ripple"),
            BALANCED_RADIX4_BRENT_KUNG => Some("balanced-radix4-brent-kung"),
            WALLACE_RADIX4_RIPPLE => Some("wallace-radix4-ripple"),
            WALLACE_RADIX4_BRENT_KUNG => Some("wallace-radix4-brent-kung"),
            DADDA_RADIX4_RIPPLE => Some("dadda-radix4-ripple"),
            DADDA_RADIX4_BRENT_KUNG => Some("dadda-radix4-brent-kung"),
            SERIAL_ARRAY_RIPPLE => Some("serial-array-ripple"),
            SERIAL_ARRAY_BRENT_KUNG => Some("serial-array-brent-kung"),
            BALANCED_ARRAY_RIPPLE => Some("balanced-array-ripple"),
            BALANCED_ARRAY_BRENT_KUNG => Some("balanced-array-brent-kung"),
            WALLACE_ARRAY_RIPPLE => Some("wallace-array-ripple"),
            WALLACE_ARRAY_BRENT_KUNG => Some("wallace-array-brent-kung"),
            DADDA_ARRAY_RIPPLE => Some("dadda-array-ripple"),
            DADDA_ARRAY_BRENT_KUNG => Some("dadda-array-brent-kung"),
            _ => None,
        }
    }

    fn module_name(&self, operator: crate::SemanticOperator) -> Option<&str> {
        match operator.kind() {
            OperatorKind::Add | OperatorKind::Sum => Some("DW01_add"),
            OperatorKind::Subtract => Some("DW01_sub"),
            OperatorKind::Increment => Some("DW01_inc"),
            OperatorKind::Decrement => Some("DW01_dec"),
            OperatorKind::Multiply
            | OperatorKind::Divide
            | OperatorKind::Modulo
            | OperatorKind::DynamicExtract => None,
        }
    }

    fn operation_mnemonic(&self, operator: crate::SemanticOperator) -> Option<&str> {
        match operator.kind() {
            OperatorKind::Add | OperatorKind::Sum | OperatorKind::Increment => Some("add"),
            OperatorKind::Subtract | OperatorKind::Decrement => Some("sub"),
            OperatorKind::Multiply
            | OperatorKind::Divide
            | OperatorKind::Modulo
            | OperatorKind::DynamicExtract => None,
        }
    }

    fn implementation_name(&self, recipe: ProviderRecipeId) -> Option<&str> {
        match recipe {
            RIPPLE_CARRY | INCREMENT | DECREMENT | CONSTANT_RIPPLE => Some("rpl"),
            BRENT_KUNG
            | KOGGE_STONE
            | HYBRID_BRENT_KUNG
            | AREA_HYBRID_BRENT_KUNG
            | CONSTANT_BRENT_KUNG => Some("cla"),
            _ => region_recipe(recipe).map(|region| match region.prefix {
                CarryNetwork::Ripple => "csa-rpl",
                CarryNetwork::BrentKung | CarryNetwork::KoggeStone => "csa-cla",
            }),
        }
    }

    fn structural_estimate(
        &self,
        recipe: ProviderRecipeId,
        operator: crate::SemanticOperator,
    ) -> Result<StructuralEstimate, crate::SynthError> {
        let width = u64::from(operator.width());
        let stages = operator.width().ilog2() + u32::from(!operator.width().is_power_of_two());
        if let Some(region) = region_recipe(recipe) {
            return arithmetic_region_structural_estimate(operator, region);
        }
        let estimate = match recipe {
            RIPPLE_CARRY => StructuralEstimate {
                logic_depth: operator.width().checked_mul(2).ok_or_else(|| {
                    crate::SynthError::invariant("ripple-carry depth estimate overflow")
                })?,
                logic_units: width.checked_mul(5).ok_or_else(|| {
                    crate::SynthError::invariant("ripple-carry logic estimate overflow")
                })?,
                wiring_units: width,
            },
            BRENT_KUNG => StructuralEstimate {
                logic_depth: stages
                    .checked_mul(2)
                    .and_then(|depth| depth.checked_add(2))
                    .ok_or_else(|| {
                        crate::SynthError::invariant("Brent-Kung depth estimate overflow")
                    })?,
                logic_units: width.checked_mul(u64::from(stages) + 3).ok_or_else(|| {
                    crate::SynthError::invariant("Brent-Kung logic estimate overflow")
                })?,
                wiring_units: width.checked_mul(u64::from(stages)).ok_or_else(|| {
                    crate::SynthError::invariant("Brent-Kung wiring estimate overflow")
                })?,
            },
            KOGGE_STONE => StructuralEstimate {
                logic_depth: stages.checked_add(2).ok_or_else(|| {
                    crate::SynthError::invariant("Kogge-Stone depth estimate overflow")
                })?,
                logic_units: width
                    .checked_mul(
                        u64::from(stages).checked_mul(2).ok_or_else(|| {
                            crate::SynthError::invariant("Kogge-Stone logic estimate overflow")
                        })? + 3,
                    )
                    .ok_or_else(|| {
                        crate::SynthError::invariant("Kogge-Stone logic estimate overflow")
                    })?,
                wiring_units: width
                    .checked_mul(u64::from(stages))
                    .and_then(|wires| wires.checked_mul(2))
                    .ok_or_else(|| {
                        crate::SynthError::invariant("Kogge-Stone wiring estimate overflow")
                    })?,
            },
            HYBRID_BRENT_KUNG => StructuralEstimate {
                logic_depth: stages
                    .checked_mul(2)
                    .and_then(|depth| depth.checked_add(4))
                    .ok_or_else(|| {
                        crate::SynthError::invariant("hybrid Brent-Kung depth estimate overflow")
                    })?,
                logic_units: width.checked_mul(u64::from(stages) + 2).ok_or_else(|| {
                    crate::SynthError::invariant("hybrid Brent-Kung logic estimate overflow")
                })?,
                wiring_units: width.checked_mul(u64::from(stages)).ok_or_else(|| {
                    crate::SynthError::invariant("hybrid Brent-Kung wiring estimate overflow")
                })?,
            },
            AREA_HYBRID_BRENT_KUNG => StructuralEstimate {
                logic_depth: stages
                    .checked_mul(2)
                    .and_then(|depth| depth.checked_add(6))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "area hybrid Brent-Kung depth estimate overflow",
                        )
                    })?,
                logic_units: width.checked_mul(u64::from(stages) + 1).ok_or_else(|| {
                    crate::SynthError::invariant("area hybrid Brent-Kung logic estimate overflow")
                })?,
                wiring_units: width.checked_mul(u64::from(stages)).ok_or_else(|| {
                    crate::SynthError::invariant("area hybrid Brent-Kung wiring estimate overflow")
                })?,
            },
            INCREMENT | DECREMENT => StructuralEstimate {
                logic_depth: operator.width().checked_mul(2).ok_or_else(|| {
                    crate::SynthError::invariant("stepper depth estimate overflow")
                })?,
                logic_units: width.checked_mul(5).ok_or_else(|| {
                    crate::SynthError::invariant("stepper logic estimate overflow")
                })?,
                wiring_units: width,
            },
            CONSTANT_RIPPLE => StructuralEstimate {
                logic_depth: operator.width().checked_mul(2).ok_or_else(|| {
                    crate::SynthError::invariant("constant ripple depth estimate overflow")
                })?,
                logic_units: width.checked_mul(2).ok_or_else(|| {
                    crate::SynthError::invariant("constant ripple logic estimate overflow")
                })?,
                wiring_units: width,
            },
            CONSTANT_BRENT_KUNG => StructuralEstimate {
                logic_depth: stages
                    .checked_mul(2)
                    .and_then(|depth| depth.checked_add(2))
                    .ok_or_else(|| {
                        crate::SynthError::invariant("constant Brent-Kung depth estimate overflow")
                    })?,
                logic_units: width.checked_mul(u64::from(stages) + 1).ok_or_else(|| {
                    crate::SynthError::invariant("constant Brent-Kung logic estimate overflow")
                })?,
                wiring_units: width.checked_mul(u64::from(stages)).ok_or_else(|| {
                    crate::SynthError::invariant("constant Brent-Kung wiring estimate overflow")
                })?,
            },
            _ => {
                return Err(crate::SynthError::invariant(format!(
                    "resource '{}' has no recipe {}",
                    self.resource_name(),
                    recipe.raw()
                )));
            }
        };
        Ok(estimate)
    }
}

impl AddSubProvider {
    fn lower<B: BitBackend>(
        &self,
        recipe: ProviderRecipeId,
        blaster: &mut BitBlaster<'_, B>,
        request: ImplementationRequest<'_>,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let [left, right] = request.operator.inputs();
        let subtract = request.operator.kind() == OperatorKind::Subtract;
        if let Some(region) = region_recipe(recipe) {
            return blaster.arithmetic_region_bits(
                request.operator,
                region,
                request.result_type,
                request.source,
            );
        }
        match recipe {
            RIPPLE_CARRY => blaster.ripple_add_sub_bits(
                left,
                right,
                subtract,
                request.result_type,
                request.source,
            ),
            BRENT_KUNG => blaster.brent_kung_add_sub_bits(
                left,
                right,
                subtract,
                request.result_type,
                request.source,
            ),
            KOGGE_STONE => blaster.kogge_stone_add_sub_bits(
                left,
                right,
                subtract,
                request.result_type,
                request.source,
            ),
            HYBRID_BRENT_KUNG => blaster.hybrid_brent_kung_add_sub_bits(
                left,
                right,
                subtract,
                request.result_type,
                request.result_type.width().isqrt().max(2),
                request.source,
            ),
            AREA_HYBRID_BRENT_KUNG => blaster.hybrid_brent_kung_add_sub_bits(
                left,
                right,
                subtract,
                request.result_type,
                request
                    .result_type
                    .width()
                    .isqrt()
                    .checked_add(1)
                    .ok_or_else(|| {
                        crate::SynthError::invariant("hybrid adder block width overflow")
                    })?,
                request.source,
            ),
            INCREMENT => blaster.constant_add_sub_bits(
                left,
                right,
                false,
                false,
                request.result_type,
                request.source,
            ),
            DECREMENT => blaster.constant_add_sub_bits(
                left,
                right,
                true,
                false,
                request.result_type,
                request.source,
            ),
            CONSTANT_RIPPLE => blaster.constant_add_sub_bits(
                left,
                right,
                subtract,
                false,
                request.result_type,
                request.source,
            ),
            CONSTANT_BRENT_KUNG => blaster.constant_add_sub_bits(
                left,
                right,
                subtract,
                true,
                request.result_type,
                request.source,
            ),
            _ => Err(crate::SynthError::invariant(format!(
                "resource '{}' has no recipe {}",
                self.resource_name(),
                recipe.raw()
            ))),
        }
    }
}

fn region_recipe(recipe: ProviderRecipeId) -> Option<RegionRecipe> {
    let (schedule, prefix, encoding) = match recipe {
        SERIAL_CSA_RIPPLE | SERIAL_RADIX4_RIPPLE => (
            CompressionSchedule::Serial,
            CarryNetwork::Ripple,
            ProductEncoding::Radix4,
        ),
        SERIAL_CSA_BRENT_KUNG | SERIAL_RADIX4_BRENT_KUNG => (
            CompressionSchedule::Serial,
            CarryNetwork::BrentKung,
            ProductEncoding::Radix4,
        ),
        BALANCED_CSA_RIPPLE | BALANCED_RADIX4_RIPPLE => (
            CompressionSchedule::Balanced,
            CarryNetwork::Ripple,
            ProductEncoding::Radix4,
        ),
        BALANCED_CSA_BRENT_KUNG | BALANCED_RADIX4_BRENT_KUNG => (
            CompressionSchedule::Balanced,
            CarryNetwork::BrentKung,
            ProductEncoding::Radix4,
        ),
        WALLACE_CSA_RIPPLE | WALLACE_RADIX4_RIPPLE => (
            CompressionSchedule::Wallace,
            CarryNetwork::Ripple,
            ProductEncoding::Radix4,
        ),
        WALLACE_CSA_BRENT_KUNG | WALLACE_RADIX4_BRENT_KUNG => (
            CompressionSchedule::Wallace,
            CarryNetwork::BrentKung,
            ProductEncoding::Radix4,
        ),
        DADDA_CSA_RIPPLE | DADDA_RADIX4_RIPPLE => (
            CompressionSchedule::Dadda,
            CarryNetwork::Ripple,
            ProductEncoding::Radix4,
        ),
        DADDA_CSA_BRENT_KUNG | DADDA_RADIX4_BRENT_KUNG => (
            CompressionSchedule::Dadda,
            CarryNetwork::BrentKung,
            ProductEncoding::Radix4,
        ),
        SERIAL_ARRAY_RIPPLE => (
            CompressionSchedule::Serial,
            CarryNetwork::Ripple,
            ProductEncoding::Array,
        ),
        SERIAL_ARRAY_BRENT_KUNG => (
            CompressionSchedule::Serial,
            CarryNetwork::BrentKung,
            ProductEncoding::Array,
        ),
        BALANCED_ARRAY_RIPPLE => (
            CompressionSchedule::Balanced,
            CarryNetwork::Ripple,
            ProductEncoding::Array,
        ),
        BALANCED_ARRAY_BRENT_KUNG => (
            CompressionSchedule::Balanced,
            CarryNetwork::BrentKung,
            ProductEncoding::Array,
        ),
        WALLACE_ARRAY_RIPPLE => (
            CompressionSchedule::Wallace,
            CarryNetwork::Ripple,
            ProductEncoding::Array,
        ),
        WALLACE_ARRAY_BRENT_KUNG => (
            CompressionSchedule::Wallace,
            CarryNetwork::BrentKung,
            ProductEncoding::Array,
        ),
        DADDA_ARRAY_RIPPLE => (
            CompressionSchedule::Dadda,
            CarryNetwork::Ripple,
            ProductEncoding::Array,
        ),
        DADDA_ARRAY_BRENT_KUNG => (
            CompressionSchedule::Dadda,
            CarryNetwork::BrentKung,
            ProductEncoding::Array,
        ),
        SERIAL_CSA_KOGGE_STONE => (
            CompressionSchedule::Serial,
            CarryNetwork::KoggeStone,
            ProductEncoding::Radix4,
        ),
        BALANCED_CSA_KOGGE_STONE => (
            CompressionSchedule::Balanced,
            CarryNetwork::KoggeStone,
            ProductEncoding::Radix4,
        ),
        WALLACE_CSA_KOGGE_STONE => (
            CompressionSchedule::Wallace,
            CarryNetwork::KoggeStone,
            ProductEncoding::Radix4,
        ),
        DADDA_CSA_KOGGE_STONE => (
            CompressionSchedule::Dadda,
            CarryNetwork::KoggeStone,
            ProductEncoding::Radix4,
        ),
        _ => return None,
    };
    Some(RegionRecipe {
        schedule,
        prefix,
        encoding,
    })
}

fn arithmetic_region_structural_estimate(
    operator: crate::SemanticOperator,
    recipe: RegionRecipe,
) -> Result<StructuralEstimate, crate::SynthError> {
    let width = u64::from(operator.width());
    let terms = u64::from(operator.term_count());
    if terms < 2 || (terms < 3 && operator.product_term_count() == 0) {
        return Err(crate::SynthError::invariant(
            "arithmetic-region recipe requires a product or at least three terms",
        ));
    }
    let negative_terms = u64::from(operator.negative_term_count());
    let product_terms = u64::from(operator.product_term_count());
    let variable_products = u64::from(operator.variable_product_term_count());
    let constant_products = product_terms.saturating_sub(variable_products);
    let scalar_rows = terms.saturating_sub(product_terms);
    let variable_rows = match recipe.encoding {
        ProductEncoding::Radix4 => width.div_ceil(2),
        ProductEncoding::Array => width,
    };
    let constant_rows = width.div_ceil(3).max(1);
    let rows = scalar_rows
        .checked_add(variable_products.saturating_mul(variable_rows))
        .and_then(|rows| rows.checked_add(constant_products.saturating_mul(constant_rows)))
        .and_then(|rows| rows.checked_add(u64::from(negative_terms != 0)))
        .ok_or_else(|| crate::SynthError::invariant("arithmetic row estimate overflow"))?;
    let input_bits = rows
        .checked_mul(width)
        .ok_or_else(|| crate::SynthError::invariant("arithmetic input estimate overflow"))?;
    let compressors = input_bits.saturating_sub(width.saturating_mul(2));
    let inversions = negative_terms
        .checked_mul(width)
        .ok_or_else(|| crate::SynthError::invariant("arithmetic inversion estimate overflow"))?;
    let mut balanced_rows = u32::try_from(rows)
        .map_err(|_| crate::SynthError::capacity("arithmetic row estimate exceeds 32 bits"))?;
    let mut balanced_levels = 0u32;
    while balanced_rows > 2 {
        balanced_rows = (balanced_rows / 3)
            .checked_mul(2)
            .and_then(|compressed| compressed.checked_add(balanced_rows % 3))
            .ok_or_else(|| crate::SynthError::invariant("compression depth estimate overflow"))?;
        balanced_levels = balanced_levels
            .checked_add(1)
            .ok_or_else(|| crate::SynthError::invariant("compression depth estimate overflow"))?;
    }
    let compression_levels = match recipe.schedule {
        CompressionSchedule::Serial => u32::try_from(rows.saturating_sub(2))
            .map_err(|_| crate::SynthError::capacity("serial compression depth exceeds 32 bits"))?,
        CompressionSchedule::Balanced | CompressionSchedule::Wallace => balanced_levels,
        CompressionSchedule::Dadda => balanced_levels.saturating_add(1),
    };
    let stages = operator.width().ilog2() + u32::from(!operator.width().is_power_of_two());
    let final_depth = match recipe.prefix {
        CarryNetwork::BrentKung => stages.checked_mul(2).and_then(|depth| depth.checked_add(2)),
        CarryNetwork::KoggeStone => stages.checked_add(2),
        CarryNetwork::Ripple => operator.width().checked_mul(2),
    }
    .ok_or_else(|| crate::SynthError::invariant("arithmetic final-adder depth overflow"))?;
    let final_units = match recipe.prefix {
        CarryNetwork::BrentKung => width.checked_mul(u64::from(stages) + 3),
        CarryNetwork::KoggeStone => width.checked_mul(u64::from(stages) * 2 + 3),
        CarryNetwork::Ripple => width.checked_mul(5),
    }
    .ok_or_else(|| crate::SynthError::invariant("arithmetic final-adder estimate overflow"))?;
    let generation_units = match recipe.encoding {
        ProductEncoding::Radix4 => variable_products
            .saturating_mul(variable_rows)
            .saturating_mul(width)
            .saturating_mul(3),
        ProductEncoding::Array => variable_products
            .saturating_mul(width)
            .saturating_mul(width),
    };
    let schedule_units = match recipe.schedule {
        CompressionSchedule::Serial | CompressionSchedule::Balanced => {
            compressors.saturating_mul(6)
        }
        CompressionSchedule::Wallace | CompressionSchedule::Dadda => compressors.saturating_mul(5),
    };
    let wiring_factor = match recipe.schedule {
        CompressionSchedule::Serial => 1,
        CompressionSchedule::Balanced | CompressionSchedule::Dadda => 2,
        CompressionSchedule::Wallace => 3,
    };
    Ok(StructuralEstimate {
        logic_depth: compression_levels
            .checked_mul(3)
            .and_then(|depth| depth.checked_add(final_depth))
            .ok_or_else(|| crate::SynthError::invariant("arithmetic depth estimate overflow"))?,
        logic_units: schedule_units
            .checked_add(inversions)
            .and_then(|units| units.checked_add(generation_units))
            .and_then(|units| units.checked_add(final_units))
            .ok_or_else(|| crate::SynthError::invariant("arithmetic logic estimate overflow"))?,
        wiring_units: input_bits
            .checked_mul(wiring_factor)
            .ok_or_else(|| crate::SynthError::invariant("arithmetic wiring estimate overflow"))?,
    })
}

impl<B: BitBackend> BitBlaster<'_, B> {
    fn arithmetic_region_bits(
        &mut self,
        operator: crate::SemanticOperator,
        recipe: RegionRecipe,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let plan = self.plan;
        let terms = plan.arithmetic_terms(operator.id());
        if terms.len() < 2
            || (terms.len() < 3 && operator.product_term_count() == 0)
            || terms.len() != operator.term_count() as usize
        {
            return Err(crate::SynthError::invariant(
                "arithmetic-region operator has an invalid term row",
            ));
        }
        let width = result_ty.width() as usize;
        let zero = self.constant(BitVal::Zero, result_ty.state(), source)?;
        let one = self.constant(BitVal::One, result_ty.state(), source)?;
        let mut matrix = BitMatrix::new(width);
        let mut constant_products = Vec::<ConstantProductGroup>::new();
        for &term in terms {
            let term = self.local_arithmetic_term(term)?;
            match term {
                ArithmeticTerm::Value {
                    value,
                    ty,
                    negative,
                } => {
                    let span = self.value(value)?;
                    let mut row = Vec::with_capacity(width);
                    for index in 0..result_ty.width() {
                        row.push(Some(self.resized_bit(span, ty, index, true, source)?));
                    }
                    if row
                        .iter()
                        .all(|bit| bit.and_then(|bit| self.scalar_constant(bit)).is_some())
                    {
                        for (column, bit) in row.into_iter().enumerate() {
                            if bit.and_then(|bit| self.scalar_constant(bit)) == Some(true) {
                                matrix.add_correction_power(column, negative);
                            }
                        }
                    } else {
                        self.append_signed_row(&mut matrix, row, negative, source)?;
                    }
                }
                term @ ArithmeticTerm::Product {
                    inputs,
                    input_types,
                    ty: _,
                    negative,
                    constant_input,
                } => {
                    if let Some(constant_input) = constant_input {
                        let constant_input = usize::from(constant_input);
                        let multiplicand_input = 1 - constant_input;
                        let constant_span = self.value(inputs[constant_input])?;
                        let mut coefficient = Vec::with_capacity(width);
                        for index in 0..result_ty.width() {
                            let bit = self.resized_bit(
                                constant_span,
                                input_types[constant_input],
                                index,
                                input_types[constant_input].is_signed(),
                                source,
                            )?;
                            coefficient.push(self.scalar_constant(bit).ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "constant arithmetic product contains an undefined bit",
                                )
                            })?);
                        }
                        let group = constant_products.iter_mut().find(|group| {
                            group.multiplicand == inputs[multiplicand_input]
                                && group.multiplicand_ty == input_types[multiplicand_input]
                        });
                        if let Some(group) = group {
                            add_modular_coefficient(&mut group.coefficient, &coefficient, negative);
                        } else {
                            let mut combined = vec![false; width];
                            add_modular_coefficient(&mut combined, &coefficient, negative);
                            constant_products.push(ConstantProductGroup {
                                multiplicand: inputs[multiplicand_input],
                                multiplicand_ty: input_types[multiplicand_input],
                                coefficient: combined,
                            });
                        }
                    } else {
                        self.append_product_rows(
                            &mut matrix,
                            term,
                            recipe.encoding,
                            result_ty,
                            source,
                        )?;
                    }
                }
            }
        }
        for group in constant_products {
            self.append_constant_coefficient_rows(
                &mut matrix,
                group.multiplicand,
                group.multiplicand_ty,
                &group.coefficient,
                result_ty,
                source,
            )?;
        }
        let carry = matrix
            .take_carry_input(
                |bit| self.scalar_constant(bit) == Some(false),
                |bit| self.backend.structural_level(bit),
            )
            .unwrap_or(zero);
        let (left, right) = self.reduce_matrix(matrix, recipe.schedule, zero, one, source)?;
        match recipe.prefix {
            CarryNetwork::BrentKung => self.brent_kung_add_vectors(&left, &right, carry, source),
            CarryNetwork::KoggeStone => self.kogge_stone_add_vectors(&left, &right, carry, source),
            CarryNetwork::Ripple => self.add_vectors(&left, &right, carry, source),
        }
    }

    fn local_arithmetic_term(
        &self,
        term: ArithmeticTerm,
    ) -> Result<ArithmeticTerm, crate::SynthError> {
        Ok(match term {
            ArithmeticTerm::Value {
                value,
                ty,
                negative,
            } => ArithmeticTerm::Value {
                value: self.local_source_value(value)?,
                ty,
                negative,
            },
            ArithmeticTerm::Product {
                inputs,
                input_types,
                ty,
                negative,
                constant_input,
            } => ArithmeticTerm::Product {
                inputs: [
                    self.local_source_value(inputs[0])?,
                    self.local_source_value(inputs[1])?,
                ],
                input_types,
                ty,
                negative,
                constant_input,
            },
        })
    }
}

fn add_modular_coefficient(accumulator: &mut [bool], operand: &[bool], negative: bool) {
    debug_assert_eq!(accumulator.len(), operand.len());
    let mut carry = negative;
    for (accumulator, &operand) in accumulator.iter_mut().zip(operand) {
        let operand = operand ^ negative;
        let sum = *accumulator ^ operand ^ carry;
        carry = (*accumulator && operand) || (carry && (*accumulator || operand));
        *accumulator = sum;
    }
}

pub(super) fn implementation_provider() -> &'static dyn ImplementationProvider {
    &AddSubProvider
}

pub(super) fn lower_implementation<B: BitBackend>(
    recipe: ProviderRecipeId,
    blaster: &mut BitBlaster<'_, B>,
    request: ImplementationRequest<'_>,
) -> Result<Vec<ScalarBit>, crate::SynthError> {
    AddSubProvider.lower(recipe, blaster, request)
}
