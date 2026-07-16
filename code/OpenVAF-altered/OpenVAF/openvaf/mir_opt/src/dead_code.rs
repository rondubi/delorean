use std::collections::VecDeque;

use bitset::BitSet;
use mir::{Function, Inst, Value, ValueDef};
use workqueue::WorkQueue;

pub fn dead_code_elimination(func: &mut Function, output_values: &BitSet<Value>) {
    // Start with an empty set instead of a filled one — avoids O(n/64) initialization
    // and prevents spurious re-processing of instructions that were never candidates.
    let mut work_list =
        WorkQueue { deque: VecDeque::new(), set: BitSet::new_empty(func.dfg.num_insts()) };

    let mut block_cursor = func.layout.rev_blocks_cursor();
    while let Some(block) = block_cursor.next(&func.layout) {
        let mut inst_cursor = func.layout.block_inst_cursor(block);
        while let Some(inst) = inst_cursor.next_back(&func.layout) {
            if func.dfg.inst_dead(inst, true)
                && !func.dfg.inst_results(inst).iter().any(|res| output_values.contains(*res))
            {
                func.dfg.zap_inst(inst);
                func.layout.remove_inst(inst);

                // arguments might be dead now
                for arg in func.dfg.instr_args(inst) {
                    if let ValueDef::Result(inst, _) = func.dfg.value_def(*arg) {
                        work_list.insert(inst);
                    }
                }
            }
        }
    }

    while let Some(inst) = work_list.take() {
        if func.layout.inst_block(inst).is_none() {
            continue; // already removed
        }
        if func.dfg.inst_dead(inst, true)
            && !func.dfg.inst_results(inst).iter().any(|res| output_values.contains(*res))
        {
            func.dfg.zap_inst(inst);
            func.layout.remove_inst(inst);

            for arg in func.dfg.instr_args(inst) {
                if let ValueDef::Result(inst, _) = func.dfg.value_def(*arg) {
                    work_list.insert(inst);
                }
            }
        }
    }
}
