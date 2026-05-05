use crate::lir::Function;
use crate::lir_forward;

pub(crate) fn simplify(function: Function) -> Function {
    if std::env::var_os("MIR_LIFT_DISABLE_LIR_OPTS").is_some() {
        return function;
    }
    lir_forward::run_forward_passes(function)
}
