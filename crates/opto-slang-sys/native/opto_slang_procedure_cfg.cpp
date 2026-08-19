// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_procedure_cfg.h"

#include <algorithm>
#include <stdexcept>
#include <utility>

namespace opto::slang_lower {

uint32_t ProcedureBuilder::add_block(OptoSlangSourceSpanView source) {
  if (blocks_.size() >= UINT32_MAX) {
    throw std::runtime_error("procedural CFG exceeds 32-bit block capacity");
  }
  const auto index = static_cast<uint32_t>(blocks_.size());
  blocks_.push_back(CfgBlock{{}, {}, source});
  return index;
}

CfgFragment ProcedureBuilder::effects(std::vector<OptoSlangEffectData> effects,
                                      OptoSlangSourceSpanView source) {
  if (effects.empty()) {
    return {};
  }
  const auto block = add_block(source);
  blocks_[block].effects = std::move(effects);
  return {block, {block}};
}

CfgFragment ProcedureBuilder::sequence(CfgFragment first, CfgFragment second,
                                       OptoSlangSourceSpanView source) {
  if (first.empty()) {
    return second;
  }
  if (second.empty()) {
    return first;
  }
  if (first.exits.empty()) {
    return first;
  }
  connect(first.exits, *second.entry, source);
  return {first.entry, std::move(second.exits)};
}

CfgFragment ProcedureBuilder::guard(const OptoSlangExpr *condition,
                                    CfgFragment body,
                                    OptoSlangSourceSpanView source) {
  if (body.empty()) {
    return {};
  }
  const auto dispatch = add_block(source);
  const auto join = add_block(source);
  branch(dispatch, condition, *body.entry, join, source);
  connect(body.exits, join, source);
  return {dispatch, {join}};
}

CfgFragment ProcedureBuilder::conditional(const OptoSlangExpr *condition,
                                          CfgFragment then_body,
                                          CfgFragment else_body,
                                          OptoSlangSourceSpanView source) {
  if (then_body.empty()) {
    if (else_body.empty()) {
      return {};
    }
  }
  const auto dispatch = add_block(source);
  const bool then_falls_through = then_body.empty() || !then_body.exits.empty();
  const bool else_falls_through = else_body.empty() || !else_body.exits.empty();
  const auto join = then_falls_through || else_falls_through
                        ? std::optional<uint32_t>(add_block(source))
                        : std::nullopt;
  branch(dispatch, condition, then_body.empty() ? *join : *then_body.entry,
         else_body.empty() ? *join : *else_body.entry, source);
  if (join) {
    connect(then_body.exits, *join, source);
  }
  if (!else_body.empty()) {
    if (join) {
      connect(else_body.exits, *join, source);
    }
  }
  return {dispatch,
          join ? std::vector<uint32_t>{*join} : std::vector<uint32_t>{}};
}

CfgFragment ProcedureBuilder::join_at(CfgFragment body, uint32_t target,
                                      OptoSlangSourceSpanView source) {
  if (target >= blocks_.size()) {
    throw std::runtime_error("procedural CFG join targets an unknown block");
  }
  if (body.empty()) {
    return {target, {target}};
  }
  for (auto exit : body.exits) {
    if (exit != target) {
      jump(exit, target, source);
    }
  }
  return {body.entry, {target}};
}

void ProcedureBuilder::jump(uint32_t from, uint32_t target,
                            OptoSlangSourceSpanView source) {
  CfgTerminator terminator;
  terminator.kind = CfgTerminatorKind::Jump;
  terminator.jump_edge = {target, source};
  terminator.source = source;
  terminate(from, std::move(terminator));
}

void ProcedureBuilder::branch(uint32_t from, const OptoSlangExpr *condition,
                              uint32_t then_target, uint32_t else_target,
                              OptoSlangSourceSpanView source) {
  CfgTerminator terminator;
  terminator.kind = CfgTerminatorKind::Branch;
  terminator.condition = condition;
  terminator.then_edge = {then_target, source};
  terminator.else_edge = {else_target, source};
  terminator.source = source;
  terminate(from, std::move(terminator));
}

void ProcedureBuilder::switch_(uint32_t from, const OptoSlangExpr *selector,
                               std::vector<OptoSlangSwitchArmData> arms,
                               uint32_t default_target,
                               OptoSlangSourceSpanView source) {
  CfgTerminator terminator;
  terminator.kind = CfgTerminatorKind::Switch;
  terminator.selector = selector;
  terminator.arms.reserve(arms.size());
  for (const auto &arm : arms) {
    terminator.arms.push_back({arm.pattern, {arm.edge.block, arm.edge.source}});
  }
  terminator.default_edge = {default_target, source};
  terminator.source = source;
  terminate(from, std::move(terminator));
}

uint32_t ProcedureBuilder::add_loop_region(OptoSlangLoopRegionData region) {
  if (loop_regions_.size() >= UINT32_MAX) {
    throw std::runtime_error(
        "procedural loop-region arena exceeds 32-bit capacity");
  }
  const auto block_count = static_cast<uint32_t>(blocks_.size());
  if (region.header >= block_count || region.body >= block_count ||
      region.latch >= block_count || region.exit >= block_count) {
    throw std::runtime_error(
        "procedural loop region references an unknown block");
  }
  if (region.parent && *region.parent >= loop_regions_.size()) {
    throw std::runtime_error(
        "procedural loop parent must be an earlier region");
  }
  const auto id = static_cast<uint32_t>(loop_regions_.size());
  loop_regions_.push_back(std::move(region));
  return id;
}

OptoSlangProcedureData
ProcedureBuilder::finish(CfgFragment body, OptoSlangProcedureKind kind,
                         std::vector<OptoSlangEventData> events,
                         OptoSlangSourceSpanView source) {
  if (body.empty()) {
    return {};
  }
  CfgTerminator terminator;
  terminator.kind = CfgTerminatorKind::Return;
  terminator.source = source;
  for (auto exit : body.exits) {
    terminate(exit, terminator);
  }
  const auto entry = prune_unreachable(*body.entry);
  validate(entry, kind, events);
  return materialize(entry, kind, std::move(events), source);
}

uint32_t ProcedureBuilder::prune_unreachable(uint32_t entry) {
  if (entry >= blocks_.size()) {
    throw std::runtime_error("procedural CFG entry block is out of range");
  }
  std::vector<bool> reached(blocks_.size());
  std::vector<uint32_t> pending{entry};
  auto enqueue = [&](CfgEdge edge) {
    if (edge.block >= blocks_.size()) {
      throw std::runtime_error("procedural CFG edge targets an unknown block");
    }
    pending.push_back(edge.block);
  };
  while (!pending.empty()) {
    const auto block = pending.back();
    pending.pop_back();
    if (reached[block]) {
      continue;
    }
    reached[block] = true;
    const auto &terminator = blocks_[block].terminator;
    switch (terminator.kind) {
    case CfgTerminatorKind::Pending:
      throw std::runtime_error(
          "reachable procedural CFG block is unterminated");
    case CfgTerminatorKind::Return:
      break;
    case CfgTerminatorKind::Jump:
      enqueue(terminator.jump_edge);
      break;
    case CfgTerminatorKind::Branch:
      enqueue(terminator.then_edge);
      enqueue(terminator.else_edge);
      break;
    case CfgTerminatorKind::Switch:
      enqueue(terminator.default_edge);
      for (const auto &arm : terminator.arms) {
        enqueue(arm.edge);
      }
      break;
    }
  }
  if (std::ranges::all_of(reached, [](bool value) { return value; })) {
    return entry;
  }

  std::vector<std::optional<uint32_t>> block_map(blocks_.size());
  std::vector<CfgBlock> compacted;
  compacted.reserve(std::ranges::count(reached, true));
  for (size_t index = 0; index < blocks_.size(); ++index) {
    if (!reached[index]) {
      continue;
    }
    if (compacted.size() >= UINT32_MAX) {
      throw std::runtime_error("procedural CFG exceeds 32-bit block capacity");
    }
    block_map[index] = static_cast<uint32_t>(compacted.size());
    compacted.push_back(std::move(blocks_[index]));
  }
  auto remap_edge = [&](CfgEdge &edge) {
    const auto mapped = block_map[edge.block];
    if (!mapped) {
      throw std::logic_error("reachable CFG edge targets a pruned block");
    }
    edge.block = *mapped;
  };
  for (auto &block : compacted) {
    auto &terminator = block.terminator;
    switch (terminator.kind) {
    case CfgTerminatorKind::Pending:
    case CfgTerminatorKind::Return:
      break;
    case CfgTerminatorKind::Jump:
      remap_edge(terminator.jump_edge);
      break;
    case CfgTerminatorKind::Branch:
      remap_edge(terminator.then_edge);
      remap_edge(terminator.else_edge);
      break;
    case CfgTerminatorKind::Switch:
      remap_edge(terminator.default_edge);
      for (auto &arm : terminator.arms) {
        remap_edge(arm.edge);
      }
      break;
    }
  }

  std::vector<std::optional<uint32_t>> region_map(loop_regions_.size());
  std::vector<OptoSlangLoopRegionData> regions;
  regions.reserve(loop_regions_.size());
  for (size_t index = 0; index < loop_regions_.size(); ++index) {
    auto region = loop_regions_[index];
    const bool header_reached = reached[region.header];
    const bool body_reached = reached[region.body];
    const bool latch_reached = reached[region.latch];
    const bool exit_reached = reached[region.exit];
    if (header_reached && body_reached && latch_reached && !exit_reached) {
      throw std::runtime_error(
          "procedural loop has no structurally reachable exit");
    }
    if (!(header_reached && body_reached && latch_reached && exit_reached)) {
      continue;
    }
    region.header = *block_map[region.header];
    region.body = *block_map[region.body];
    region.latch = *block_map[region.latch];
    region.exit = *block_map[region.exit];
    auto parent = region.parent;
    while (parent && !region_map[*parent]) {
      parent = loop_regions_[*parent].parent;
    }
    region.parent = parent ? region_map[*parent] : std::nullopt;
    region_map[index] = static_cast<uint32_t>(regions.size());
    regions.push_back(std::move(region));
  }
  blocks_ = std::move(compacted);
  loop_regions_ = std::move(regions);
  return *block_map[entry];
}

void ProcedureBuilder::validate(
    uint32_t entry, OptoSlangProcedureKind kind,
    const std::vector<OptoSlangEventData> &events) const {
  if ((kind == OPTO_SLANG_PROCEDURE_FLOP) != !events.empty()) {
    throw std::runtime_error(
        "procedure kind and sensitivity events are inconsistent");
  }
  std::vector<bool> reached(blocks_.size());
  std::vector<uint32_t> pending{entry};
  auto push = [&](CfgEdge edge) {
    if (edge.block >= blocks_.size()) {
      throw std::runtime_error("procedural CFG edge targets an unknown block");
    }
    pending.push_back(edge.block);
  };
  while (!pending.empty()) {
    const auto block_index = pending.back();
    pending.pop_back();
    if (block_index >= blocks_.size()) {
      throw std::runtime_error("procedural CFG edge targets an unknown block");
    }
    if (reached[block_index]) {
      continue;
    }
    reached[block_index] = true;
    const auto &block = blocks_[block_index];
    if (std::ranges::any_of(block.effects, [](const auto &effect) {
          return !effect.lhs || !effect.rhs;
        })) {
      throw std::runtime_error("procedural CFG contains an incomplete effect");
    }
    const auto &terminator = block.terminator;
    switch (terminator.kind) {
    case CfgTerminatorKind::Pending:
      throw std::runtime_error("procedural CFG contains an unterminated block");
    case CfgTerminatorKind::Return:
      break;
    case CfgTerminatorKind::Jump:
      push(terminator.jump_edge);
      break;
    case CfgTerminatorKind::Branch:
      if (!terminator.condition) {
        throw std::runtime_error("procedural branch has no condition");
      }
      push(terminator.else_edge);
      push(terminator.then_edge);
      break;
    case CfgTerminatorKind::Switch:
      if (!terminator.selector || terminator.arms.empty()) {
        throw std::runtime_error("procedural switch is incomplete");
      }
      push(terminator.default_edge);
      for (auto arm = terminator.arms.rbegin(); arm != terminator.arms.rend();
           ++arm) {
        if (!arm->pattern) {
          throw std::runtime_error("procedural switch arm has no pattern");
        }
        push(arm->edge);
      }
      break;
    }
  }
  if (std::ranges::find(reached, false) != reached.end()) {
    throw std::runtime_error("procedural CFG contains an unreachable block");
  }
}

OptoSlangProcedureData
ProcedureBuilder::materialize(uint32_t entry, OptoSlangProcedureKind kind,
                              std::vector<OptoSlangEventData> events,
                              OptoSlangSourceSpanView source) {
  OptoSlangProcedureData procedure;
  procedure.kind = kind;
  procedure.events = std::move(events);
  procedure.loop_regions = std::move(loop_regions_);
  procedure.entry_block = entry;
  procedure.source = source;
  procedure.blocks.reserve(blocks_.size());
  for (auto &block : blocks_) {
    OptoSlangBlockData published;
    published.effects = std::move(block.effects);
    published.terminated = true;
    published.source = block.source;
    auto &terminator = block.terminator;
    auto &output = published.terminator;
    output.condition = terminator.condition;
    output.selector = terminator.selector;
    output.jump_edge = {terminator.jump_edge.block,
                        terminator.jump_edge.source};
    output.then_edge = {terminator.then_edge.block,
                        terminator.then_edge.source};
    output.else_edge = {terminator.else_edge.block,
                        terminator.else_edge.source};
    output.default_edge = {terminator.default_edge.block,
                           terminator.default_edge.source};
    output.source = terminator.source;
    switch (terminator.kind) {
    case CfgTerminatorKind::Pending:
      throw std::logic_error("cannot materialize an unterminated CFG block");
    case CfgTerminatorKind::Return:
      output.kind = OPTO_SLANG_TERMINATOR_RETURN;
      break;
    case CfgTerminatorKind::Jump:
      output.kind = OPTO_SLANG_TERMINATOR_JUMP;
      break;
    case CfgTerminatorKind::Branch:
      output.kind = OPTO_SLANG_TERMINATOR_BRANCH;
      break;
    case CfgTerminatorKind::Switch:
      output.kind = OPTO_SLANG_TERMINATOR_SWITCH;
      output.arms.reserve(terminator.arms.size());
      for (const auto &arm : terminator.arms) {
        output.arms.push_back({arm.pattern, {arm.edge.block, arm.edge.source}});
      }
      break;
    }
    procedure.blocks.push_back(std::move(published));
  }
  return procedure;
}

void ProcedureBuilder::connect(const std::vector<uint32_t> &exits,
                               uint32_t target,
                               OptoSlangSourceSpanView source) {
  for (auto exit : exits) {
    jump(exit, target, source);
  }
}

void ProcedureBuilder::terminate(uint32_t block, CfgTerminator terminator) {
  if (block >= blocks_.size() ||
      blocks_[block].terminator.kind != CfgTerminatorKind::Pending) {
    throw std::runtime_error(
        "procedural CFG block is invalid or already terminated");
  }
  blocks_[block].terminator = std::move(terminator);
}

} // namespace opto::slang_lower
