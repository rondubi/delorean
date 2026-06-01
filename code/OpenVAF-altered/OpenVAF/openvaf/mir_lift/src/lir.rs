use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    pub functions: Vec<Function>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub name: String,
    pub params: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub local_variable_touched: HashMap<LocalId, bool>,
    pub local_generic_temp_name: HashMap<LocalId, bool>,
    pub entry: Label,
    pub blocks: Vec<Block>,
    pub returns: Vec<ReturnSlot>,
    pub output_types: HashMap<String, LirType>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Local {
    pub id: LocalId,
    pub name_hint: String,
    pub ty: LirType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LirType {
    Bool,
    Int,
    Real,
    Str,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub label: Label,
    pub stmts: Vec<Stmt>,
    pub term: Terminator,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Assign { dst: LocalId, value: Expr },
    Capture { key: String, value: Expr },
    CallEffect(CallEffect),
    Expr(Expr),
    Unsupported { dsts: Vec<LocalId>, text: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum CallEffect {
    Diagnostic { target: String, args: Vec<Expr> },
    SetInvalidParam { param: String },
    CollapseHint { hi: String, lo: Option<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    Goto(Label),
    Branch { cond: Expr, then_label: Label, else_label: Label },
    Return(Vec<ReturnValue>),
    Unreachable,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Local(LocalId),
    Const(ConstValue),
    Unary { op: UnaryOp, arg: Box<Expr> },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Abs { arg: Box<Expr> },
    Max { lhs: Box<Expr>, rhs: Box<Expr> },
    Min { lhs: Box<Expr>, rhs: Box<Expr> },
    SimparamOpt { name: Box<Expr>, default: Box<Expr> },
    Call { target: String, args: Vec<Expr> },
    Unsupported { text: String, args: Vec<Expr> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConstValue {
    Bool(bool),
    Int(i32),
    Real(f64),
    Str(String),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Not,
    Neg,
    Cast(LirType),
    Math1(MathUnary),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Math2(MathBinary),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MathUnary {
    Sqrt,
    Exp,
    Ln,
    Log10,
    Clog2,
    Floor,
    Ceil,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MathBinary {
    Hypot,
    Atan2,
    Pow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnSlot {
    pub key: String,
    pub value: LocalId,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReturnValue {
    Named { key: String, value: Expr },
}

impl Function {
    pub fn block(&self, label: Label) -> Option<&Block> {
        self.blocks.iter().find(|block| block.label == label)
    }

    pub fn local(&self, id: LocalId) -> Option<&Local> {
        self.locals.get(id.0).filter(|local| local.id == id)
    }
}

impl fmt::Display for LocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "l{}", self.0)
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn {}(", self.name)?;
        for (index, param) in self.params.iter().enumerate() {
            if index != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", self.local_name(*param))?;
        }
        writeln!(f, ") {{")?;
        for block in &self.blocks {
            writeln!(f, "  {}:", block.label)?;
            for stmt in &block.stmts {
                writeln!(f, "    {stmt}")?;
            }
            writeln!(f, "    {}", block.term)?;
        }
        writeln!(f, "}}")
    }
}

impl Function {
    fn local_name(&self, local: LocalId) -> String {
        self.local(local).map(|local| local.name_hint.clone()).unwrap_or_else(|| local.to_string())
    }
}

impl fmt::Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stmt::Assign { dst, value } => write!(f, "{dst} = {value};"),
            Stmt::Capture { key, value } => write!(f, "capture {key:?} = {value};"),
            Stmt::CallEffect(effect) => write!(f, "{effect};"),
            Stmt::Expr(value) => write!(f, "{value};"),
            Stmt::Unsupported { dsts, text } if dsts.is_empty() => {
                write!(f, "unsupported {text:?};")
            }
            Stmt::Unsupported { dsts, text } => {
                for (index, dst) in dsts.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{dst}")?;
                }
                write!(f, " = unsupported {text:?};")
            }
        }
    }
}

impl fmt::Display for CallEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallEffect::Diagnostic { target, args } => {
                write!(f, "diagnostic {target:?}(")?;
                for (index, arg) in args.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            CallEffect::SetInvalidParam { param } => {
                write!(f, "set_invalid_param({param})")
            }
            CallEffect::CollapseHint { hi, lo: Some(lo) } => {
                write!(f, "collapse_hint({hi}, {lo})")
            }
            CallEffect::CollapseHint { hi, lo: None } => {
                write!(f, "collapse_hint({hi}, none)")
            }
        }
    }
}

impl fmt::Display for Terminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Goto(label) => write!(f, "goto {label};"),
            Terminator::Branch { cond, then_label, else_label } => {
                write!(f, "if {cond} goto {then_label} else {else_label};")
            }
            Terminator::Return(values) => {
                write!(f, "return {{")?;
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    match value {
                        ReturnValue::Named { key, value } => write!(f, "{key:?}: {value}")?,
                    }
                }
                write!(f, "}};")
            }
            Terminator::Unreachable => write!(f, "unreachable;"),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Local(local) => write!(f, "{local}"),
            Expr::Const(value) => write!(f, "{value}"),
            Expr::Unary { op, arg } => write!(f, "{op}({arg})"),
            Expr::Binary { op, lhs, rhs } => write!(f, "({lhs} {op} {rhs})"),
            Expr::Abs { arg } => write!(f, "abs({arg})"),
            Expr::Max { lhs, rhs } => write!(f, "max({lhs}, {rhs})"),
            Expr::Min { lhs, rhs } => write!(f, "min({lhs}, {rhs})"),
            Expr::SimparamOpt { name, default } => {
                write!(f, "simparam_opt({name}, {default})")
            }
            Expr::Call { target, args } => {
                write!(f, "{target}(")?;
                for (index, arg) in args.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            Expr::Unsupported { text, args } => {
                write!(f, "unsupported({text:?}")?;
                for arg in args {
                    write!(f, ", {arg}")?;
                }
                write!(f, ")")
            }
        }
    }
}

impl fmt::Display for ConstValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConstValue::Bool(value) => write!(f, "{value}"),
            ConstValue::Int(value) => write!(f, "{value}"),
            ConstValue::Real(value) => write!(f, "{value}"),
            ConstValue::Str(value) => write!(f, "{value:?}"),
            ConstValue::None => write!(f, "none"),
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOp::Not => write!(f, "!"),
            UnaryOp::Neg => write!(f, "-"),
            UnaryOp::Cast(ty) => write!(f, "cast<{ty:?}>"),
            UnaryOp::Math1(op) => write!(f, "{op:?}"),
        }
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOp::Add => write!(f, "+"),
            BinaryOp::Sub => write!(f, "-"),
            BinaryOp::Mul => write!(f, "*"),
            BinaryOp::Div => write!(f, "/"),
            BinaryOp::Rem => write!(f, "%"),
            BinaryOp::Shl => write!(f, "<<"),
            BinaryOp::Shr => write!(f, ">>"),
            BinaryOp::BitAnd => write!(f, "&"),
            BinaryOp::BitOr => write!(f, "|"),
            BinaryOp::BitXor => write!(f, "^"),
            BinaryOp::Eq => write!(f, "=="),
            BinaryOp::Ne => write!(f, "!="),
            BinaryOp::Lt => write!(f, "<"),
            BinaryOp::Le => write!(f, "<="),
            BinaryOp::Gt => write!(f, ">"),
            BinaryOp::Ge => write!(f, ">="),
            BinaryOp::Math2(op) => write!(f, "{op:?}"),
        }
    }
}
