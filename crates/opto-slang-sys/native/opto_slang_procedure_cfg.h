// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#pragma once

#include "opto_slang_internal.h"

#include <cstdint>
#include <optional>
#include <utility>
#include <vector>

namespace opto::slang_lower {

// A fragment is an open subgraph: entry identifies its first block and exits
// are the still-unterminated blocks that accept the next sequential fragment.
// Block identities belong to exactly one ProcedureBuilder arena.
struct CfgFragment {
  std::optional<uint32_t> entry;
  std::vector<uint32_t> exits;

  bool empty() const { return !entry; }
};

enum class CfgTerminatorKind : uint8_t {
  Pending,
  Return,
  Jump,
  Branch,
  Switch,
};

struct CfgEdge {
  uint32_t block = 0;
  OptoSlangSourceSpanView source{};
};

struct CfgSwitchArm {
  const OptoSlangExpr *pattern = nullptr;
  CfgEdge edge;
};

struct CfgTerminator {
  CfgTerminatorKind kind = CfgTerminatorKind::Pending;
  const OptoSlangExpr *condition = nullptr;
  const OptoSlangExpr *selector = nullptr;
  CfgEdge jump_edge;
  CfgEdge then_edge;
  CfgEdge else_edge;
  CfgEdge default_edge;
  std::vector<CfgSwitchArm> arms;
  OptoSlangSourceSpanView source{};
};

struct CfgBlock {
  std::vector<OptoSlangEffectData> effects;
  CfgTerminator terminator;
  OptoSlangSourceSpanView source{};
};

// Owns the transient procedural CFG. Source lowering may construct arbitrary
// control flow in this arena, including loop backedges. Publication requires a
// reachable graph and preserves canonical loop-region metadata. Terminal source
// control can leave preallocated continuation blocks unreachable; `finish`
// compacts those blocks and any loop regions that no longer contain a backedge
// before publication. The Rust transient Proc pipeline owns proof and backedge
// elimination for the remaining regions.
class ProcedureBuilder {
public:
  uint32_t add_block(OptoSlangSourceSpanView source);

  CfgFragment effects(std::vector<OptoSlangEffectData> effects,
                      OptoSlangSourceSpanView source);

  CfgFragment sequence(CfgFragment first, CfgFragment second,
                       OptoSlangSourceSpanView source);

  CfgFragment guard(const OptoSlangExpr *condition, CfgFragment body,
                    OptoSlangSourceSpanView source);

  CfgFragment conditional(const OptoSlangExpr *condition, CfgFragment then_body,
                          CfgFragment else_body,
                          OptoSlangSourceSpanView source);

  // Connects every ordinary fallthrough edge to `target` while preserving
  // transfers that already terminated elsewhere.
  CfgFragment join_at(CfgFragment body, uint32_t target,
                      OptoSlangSourceSpanView source);

  void jump(uint32_t from, uint32_t target, OptoSlangSourceSpanView source);

  void branch(uint32_t from, const OptoSlangExpr *condition,
              uint32_t then_target, uint32_t else_target,
              OptoSlangSourceSpanView source);

  void switch_(uint32_t from, const OptoSlangExpr *selector,
               std::vector<OptoSlangSwitchArmData> arms,
               uint32_t default_target, OptoSlangSourceSpanView source);

  uint32_t add_loop_region(OptoSlangLoopRegionData region);

  OptoSlangProcedureData finish(CfgFragment body, OptoSlangProcedureKind kind,
                                std::vector<OptoSlangEventData> events,
                                OptoSlangSourceSpanView source);

private:
  uint32_t prune_unreachable(uint32_t entry);
  void validate(uint32_t entry, OptoSlangProcedureKind kind,
                const std::vector<OptoSlangEventData> &events) const;
  OptoSlangProcedureData materialize(uint32_t entry,
                                     OptoSlangProcedureKind kind,
                                     std::vector<OptoSlangEventData> events,
                                     OptoSlangSourceSpanView source);
  void connect(const std::vector<uint32_t> &exits, uint32_t target,
               OptoSlangSourceSpanView source);
  void terminate(uint32_t block, CfgTerminator terminator);

  std::vector<CfgBlock> blocks_;
  std::vector<OptoSlangLoopRegionData> loop_regions_;
};

} // namespace opto::slang_lower
