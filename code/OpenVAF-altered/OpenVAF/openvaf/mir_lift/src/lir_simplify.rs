use crate::lir::Function;
use crate::lir_forward;

pub(crate) fn simplify(function: Function) -> Function {
    lir_forward::run_forward_passes(function)
}
