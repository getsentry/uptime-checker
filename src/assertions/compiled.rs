use http::HeaderValue;
use jsonpath_rust::{
    parser::{errors::JsonPathError, model::JpQuery, parse_json_path},
    query::js_path_process,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::{Borrow, BorrowMut},
    num::{ParseFloatError, ParseIntError},
    time::Instant,
};

const GLOB_COMPLEXITY_LIMIT: u64 = 20;

#[derive(thiserror::Error, Debug, Serialize)]
pub enum RuntimeError {
    #[error("Invalid JSONPath: {msg}")]
    InvalidJsonPath { msg: String },

    #[error("Assertion took too long")]
    TookTooLong,

    #[error("Invalid Body JSON: {body}")]
    InvalidBodyJson { body: String },

    #[error("Invalid type in comparison: {msg}")]
    InvalidTypeComparison { msg: String },
}

#[derive(thiserror::Error, Debug, Serialize)]
pub enum CompilationError {
    #[error("Invalid glob {glob}: {msg}")]
    InvalidGlob { glob: String, msg: String },

    #[error("JSONPath Parser Error: {msg}")]
    JSONPathParser {
        msg: String,
        path: String,
        pos: usize,
    },

    #[error("Invalid JSONPath: {msg}")]
    InvalidJsonPath { msg: String },

    #[error("Too many assertion operations")]
    TooManyOperations,
}

// This struct records the reason behind the result of an assertion evaluation.  Eventually,
// an assert evaluates to 'true' or 'false', but we won't know whether we're failing until
// we're done evaluating.  To keep track of errors, we therefore keep track of the result of
// given subtree of an assertion--in the case of nodes that have more than one child, we pick
// the path that resulted in our decision, which could be all children (in the case of a
// passing AND, or a failing OR), or a single child (in the case of a failing AND or passing
// OR).  It can also be a leaf value.
pub struct EvalResult {
    pub reason_path: EvalPath,
    pub result: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalPath {
    Leaf,
    AllChildren,
    ChildIndex { index: usize, child: Box<EvalPath> },
}

impl EvalResult {
    pub fn and_node() -> EvalResult {
        Self {
            reason_path: EvalPath::AllChildren,
            result: true,
        }
    }

    pub fn or_node() -> EvalResult {
        Self {
            reason_path: EvalPath::AllChildren,
            result: false,
        }
    }

    // Construct a leaf node that is directly responsible for the result.
    pub fn leaf(value: bool) -> EvalResult {
        Self {
            reason_path: EvalPath::Leaf,
            result: value,
        }
    }

    // Construct a node in which a single child node is responsible for
    // the result.
    pub fn child_idx(index: usize, value: bool, path: EvalPath) -> EvalResult {
        Self {
            reason_path: EvalPath::ChildIndex {
                index,
                child: path.into(),
            },
            result: value,
        }
    }
}

pub fn extract_failure_data(
    path: &EvalPath,
    assert_op: &crate::assertions::Op,
) -> crate::assertions::Op {
    match path {
        EvalPath::Leaf => assert_op.clone(),
        EvalPath::AllChildren => assert_op.clone(),
        EvalPath::ChildIndex { index, child } => match assert_op {
            crate::assertions::Op::And { children } => crate::assertions::Op::And {
                children: vec![extract_failure_data(child, &children[*index])],
            },
            crate::assertions::Op::Or { children } => crate::assertions::Op::Or {
                children: vec![extract_failure_data(child, &children[*index])],
            },
            crate::assertions::Op::Not { operand } => crate::assertions::Op::Not {
                operand: extract_failure_data(child, operand).into(),
            },
            _ => panic!(),
        },
    }
}

struct Gas(u32);

impl Gas {
    fn consume(&mut self, amount: u32) -> Result<(), RuntimeError> {
        if amount > self.0 {
            return Err(RuntimeError::TookTooLong);
        }
        self.0 -= amount;

        Ok(())
    }
}

impl Borrow<u32> for Gas {
    fn borrow(&self) -> &u32 {
        &self.0
    }
}

impl BorrowMut<u32> for Gas {
    fn borrow_mut(&mut self) -> &mut u32 {
        &mut self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Assertion {
    root: Op,
}

impl Assertion {
    pub fn eval(
        &self,
        status_code: u16,
        headers: &hyper::header::HeaderMap<HeaderValue>,
        body: &[u8],
        assertion_complexity: u32,
        region: &'static str,
    ) -> Result<EvalResult, RuntimeError> {
        let start = Instant::now();
        let mut gas = Gas(assertion_complexity);
        let result = self.root.eval(status_code, headers, body, &mut gas);

        metrics::histogram!(
            "assertion.eval",
            "histogram" => "timer",
            "uptime_region" => region,
        )
        .record(start.elapsed().as_secs_f64());

        result
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Comparison {
    Always,
    Never,
    LessThan,
    GreaterThan,
    Equal,
    NotEqual,
}

impl From<&super::Comparison> for Comparison {
    fn from(value: &super::Comparison) -> Self {
        match *value {
            super::Comparison::Always => Comparison::Always,
            super::Comparison::Never => Comparison::Never,
            super::Comparison::LessThan => Comparison::LessThan,
            super::Comparison::GreaterThan => Comparison::GreaterThan,
            super::Comparison::Equals => Comparison::Equal,
            super::Comparison::NotEqual => Comparison::NotEqual,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum JSONPathOperand {
    None,
    Literal { value: String },
    Glob { pattern: relay_pattern::Pattern },
}
impl JSONPathOperand {
    fn eval(
        &self,
        op: &Comparison,
        result: Vec<jsonpath_rust::query::QueryRef<'_, serde_json::Value>>,
        gas: &mut Gas,
    ) -> Result<bool, RuntimeError> {
        // No results explicitly implies 'false', in this case.
        if result.is_empty() {
            return Ok(false);
        }

        for res in result {
            let v = res.val();

            let result = match op {
                Comparison::Always => true,
                Comparison::Never => false,
                Comparison::LessThan => match self {
                    JSONPathOperand::None => false,
                    JSONPathOperand::Literal { value } => cmp_lt_json(v, value, gas)?,

                    JSONPathOperand::Glob { .. } => false,
                },
                Comparison::GreaterThan => match self {
                    JSONPathOperand::None => false,
                    JSONPathOperand::Literal { value } => cmp_gt_json(v, value, gas)?,

                    JSONPathOperand::Glob { .. } => false,
                },
                Comparison::Equal => cmp_eq_jsonpath(v, self, gas)?,
                Comparison::NotEqual => !cmp_eq_jsonpath(v, self, gas)?,
            };

            if !result {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

impl TryFrom<&super::JSONPathOperand> for JSONPathOperand {
    type Error = CompilationError;

    fn try_from(value: &super::JSONPathOperand) -> Result<Self, Self::Error> {
        let v = match value {
            super::JSONPathOperand::None => JSONPathOperand::None,
            super::JSONPathOperand::Literal { value } => JSONPathOperand::Literal {
                value: value.clone(),
            },
            super::JSONPathOperand::Glob { pattern: value } => {
                let r = relay_pattern::Pattern::builder(&value.value)
                    .max_complexity(GLOB_COMPLEXITY_LIMIT)
                    .build();

                match r {
                    Ok(value) => JSONPathOperand::Glob { pattern: value },
                    Err(e) => {
                        return Err(CompilationError::InvalidGlob {
                            glob: e.to_string(),
                            msg: e.to_string(),
                        })
                    }
                }
            }
        };
        Ok(v)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum HeaderOperand {
    None,
    Literal { value: Value },
    Glob { value: relay_pattern::Pattern },
}

impl HeaderOperand {
    pub fn eval(
        &self,
        op: &Comparison,
        header_value: &str,
        gas: &mut Gas,
    ) -> Result<bool, RuntimeError> {
        let result = match op {
            Comparison::Always => true,
            Comparison::Never => false,
            Comparison::LessThan => match self {
                HeaderOperand::None => false,
                HeaderOperand::Literal { value } => match value {
                    Value::I64(value) => cmp_lt(&header_value.parse()?, value, gas)?,
                    Value::F64(value) => cmp_lt(&header_value.parse()?, value, gas)?,
                    Value::String(value) => cmp_lt(header_value, value.as_str(), gas)?,
                },
                HeaderOperand::Glob { .. } => false,
            },
            Comparison::GreaterThan => match self {
                HeaderOperand::None => false,
                HeaderOperand::Literal { value } => match value {
                    Value::I64(value) => cmp_gt(&header_value.parse()?, value, gas)?,
                    Value::F64(value) => cmp_gt(&header_value.parse()?, value, gas)?,
                    Value::String(value) => cmp_gt(header_value, value, gas)?,
                },
                HeaderOperand::Glob { .. } => false,
            },
            Comparison::Equal => cmp_eq_header(header_value, self, gas)?,
            Comparison::NotEqual => !cmp_eq_header(header_value, self, gas)?,
        };

        Ok(result)
    }
}

impl TryFrom<&super::HeaderOperand> for HeaderOperand {
    type Error = CompilationError;
    fn try_from(value: &super::HeaderOperand) -> Result<Self, Self::Error> {
        let v = match value {
            super::HeaderOperand::None => HeaderOperand::None,
            super::HeaderOperand::Literal { value } => HeaderOperand::Literal {
                value: value.into(),
            },
            super::HeaderOperand::Glob { pattern: value } => {
                let r = relay_pattern::Pattern::builder(&value.value)
                    .max_complexity(GLOB_COMPLEXITY_LIMIT)
                    .build();

                match r {
                    Ok(value) => HeaderOperand::Glob { value },
                    Err(e) => {
                        return Err(CompilationError::InvalidGlob {
                            glob: e.to_string(),
                            msg: e.to_string(),
                        })
                    }
                }
            }
        };

        Ok(v)
    }
}

#[derive(Debug, PartialEq)]
enum Value {
    I64(i64),
    F64(f64),
    String(String),
}

impl Eq for Value {}

impl From<&String> for Value {
    fn from(value: &String) -> Self {
        let f = value.parse::<f64>();
        if let Ok(f) = f {
            return Value::F64(f);
        }

        let i = value.parse::<i64>();
        if let Ok(i) = i {
            return Value::I64(i);
        }

        Value::String(value.clone())
    }
}

fn cmp_eq<T>(response_value: &T, test_value: &T, gas: &mut Gas) -> Result<bool, RuntimeError>
where
    T: PartialEq + ?Sized,
{
    gas.consume(1)?;
    Ok(*response_value == *test_value)
}

fn cmp_coerce_json<FF64, FI64, FSTR>(
    json_value: &serde_json::Value,
    test_value: &String,
    gas: &mut Gas,
    cmp_f64: FF64,
    cmp_i64: FI64,
    cmp_str: FSTR,
) -> Result<bool, RuntimeError>
where
    FF64: FnOnce(&f64, &f64, &mut Gas) -> Result<bool, RuntimeError>,
    FI64: FnOnce(&i64, &i64, &mut Gas) -> Result<bool, RuntimeError>,
    FSTR: FnOnce(&str, &str, &mut Gas) -> Result<bool, RuntimeError>,
{
    if let Some(v) = json_value.as_f64() {
        if let Ok(tv) = test_value.parse() {
            return cmp_f64(&v, &tv, gas);
        }
    }

    if let Some(v) = json_value.as_i64() {
        if let Ok(tv) = test_value.parse() {
            return cmp_i64(&v, &tv, gas);
        }
    }

    if let Some(v) = json_value.as_str() {
        return cmp_str(v, test_value, gas);
    }

    Err(RuntimeError::InvalidTypeComparison {
        msg: format!(
            "could not coerce a comparison between {} and {}",
            json_value, test_value
        ),
    })
}

fn cmp_lt_json(
    json_value: &serde_json::Value,
    test_value: &String,
    gas: &mut Gas,
) -> Result<bool, RuntimeError> {
    cmp_coerce_json(json_value, test_value, gas, cmp_lt, cmp_lt, cmp_lt)
}

fn cmp_lt<T>(response_value: &T, test_value: &T, gas: &mut Gas) -> Result<bool, RuntimeError>
where
    T: PartialOrd + ?Sized,
{
    gas.consume(1)?;
    Ok(*response_value < *test_value)
}

fn cmp_gt_json(
    json_value: &serde_json::Value,
    test_value: &String,
    gas: &mut Gas,
) -> Result<bool, RuntimeError> {
    cmp_coerce_json(json_value, test_value, gas, cmp_gt, cmp_gt, cmp_gt)
}

fn cmp_gt<T>(response_value: &T, test_value: &T, gas: &mut Gas) -> Result<bool, RuntimeError>
where
    T: PartialOrd + ?Sized,
{
    gas.consume(1)?;
    Ok(*response_value > *test_value)
}

fn cmp_eq_json(
    json_value: &serde_json::Value,
    test_value: &String,
    gas: &mut Gas,
) -> Result<bool, RuntimeError> {
    cmp_coerce_json(json_value, test_value, gas, cmp_eq, cmp_eq, cmp_eq)
}

fn cmp_eq_header(
    header_value: &str,
    test_value: &HeaderOperand,
    gas: &mut Gas,
) -> Result<bool, RuntimeError> {
    let result = match test_value {
        HeaderOperand::Literal { value } => match value {
            Value::I64(value) => cmp_eq(&header_value.parse()?, value, gas)?,
            Value::F64(value) => cmp_eq(&header_value.parse()?, value, gas)?,
            Value::String(value) => cmp_eq(header_value, value.as_str(), gas)?,
        },
        HeaderOperand::Glob { value } => {
            // TODO: either expose, or copy, the complexity metric from relay-pattern.
            gas.consume(5)?;
            value.is_match(header_value)
        }
        HeaderOperand::None => false,
    };

    Ok(result)
}

fn cmp_eq_jsonpath(
    body_value: &serde_json::Value,
    test_value: &JSONPathOperand,
    gas: &mut Gas,
) -> Result<bool, RuntimeError> {
    let result = match test_value {
        JSONPathOperand::Literal { value } => cmp_eq_json(body_value, value, gas)?,
        JSONPathOperand::Glob { pattern } => {
            // TODO: either expose, or copy, the complexity metric from relay-pattern.
            gas.consume(5)?;
            pattern.is_match(body_value.as_str().ok_or(RuntimeError::TookTooLong)?)
        }
        JSONPathOperand::None => false,
    };

    Ok(result)
}

#[derive(Debug, PartialEq)]
enum Op {
    And {
        children: Vec<Op>,
    },
    Or {
        children: Vec<Op>,
    },
    Not {
        operand: Box<Op>,
    },
    StatusCodeCheck {
        value: u16,
        operator: Comparison,
    },
    JsonPath {
        value: JpQuery,
        operator: Comparison,
        operand: JSONPathOperand,
    },
    HeaderCheck {
        key_op: Comparison,
        key_operand: HeaderOperand,
        value_op: Comparison,
        value_operand: HeaderOperand,
    },
}

impl Eq for Op {}

impl Op {
    fn eval(
        &self,
        status_code: u16,
        headers: &hyper::header::HeaderMap<HeaderValue>,
        body: &[u8],
        gas: &mut Gas,
    ) -> Result<EvalResult, RuntimeError> {
        let result = match self {
            Op::And { children } => {
                let mut result = EvalResult::and_node();
                for (idx, child) in children.iter().enumerate() {
                    let eval = child.eval(status_code, headers, body, gas)?;
                    if !eval.result {
                        result = EvalResult::child_idx(idx, eval.result, eval.reason_path);
                        break;
                    }
                }
                result
            }
            Op::Or { children } => {
                let mut result = EvalResult::or_node();
                for (idx, child) in children.iter().enumerate() {
                    let eval = child.eval(status_code, headers, body, gas)?;
                    if eval.result {
                        result = EvalResult::child_idx(idx, eval.result, eval.reason_path);
                        break;
                    }
                }
                result
            }
            Op::Not { operand } => {
                let eval = operand.eval(status_code, headers, body, gas)?;
                EvalResult::child_idx(0, !eval.result, eval.reason_path)
            }
            Op::StatusCodeCheck { value, operator } => {
                gas.consume(1)?;
                let value = match operator {
                    Comparison::Always => true,
                    Comparison::Never => false,
                    Comparison::LessThan => status_code < *value,
                    Comparison::GreaterThan => status_code > *value,
                    Comparison::Equal => *value == status_code,
                    Comparison::NotEqual => *value != status_code,
                };
                EvalResult::leaf(value)
            }
            Op::HeaderCheck {
                key_op,
                key_operand,
                value_op,
                value_operand,
            } => {
                // Find any header that passes key.  Then, see if it's value passes value.
                let mut result = EvalResult::leaf(false);

                for (k, v) in headers {
                    if key_operand.eval(key_op, k.as_str(), gas)? {
                        if let Ok(value_str) = v.to_str() {
                            if value_operand.eval(value_op, value_str, gas)? {
                                result = EvalResult::leaf(true);
                                break;
                            }
                        }
                    }
                }

                result
            }
            Op::JsonPath {
                value,
                operand,
                operator,
            } => {
                let json: serde_json::Value =
                    serde_json::from_slice(body).map_err(|e| RuntimeError::InvalidBodyJson {
                        body: e.to_string(),
                    })?;

                let result = js_path_process(value, &json, gas.borrow_mut())?;

                let v = operand.eval(operator, result, gas)?;

                EvalResult::leaf(v)
            }
        };

        Ok(result)
    }
}

fn dec_ops(num_ops: &mut u32) -> Result<(), CompilationError> {
    if *num_ops > 0 {
        *num_ops -= 1;
        return Ok(());
    }

    Err(CompilationError::TooManyOperations)
}

pub fn compile(
    assertion: &super::Assertion,
    max_assertion_ops: u32,
    region: &'static str,
) -> Result<Assertion, CompilationError> {
    let mut num_ops = max_assertion_ops;
    let start = Instant::now();
    let compiled = compile_op(&assertion.root, &mut num_ops);

    metrics::histogram!(
        "assertion.compile",
        "histogram" => "timer",
        "uptime_region" => region,
    )
    .record(start.elapsed().as_secs_f64());

    Ok(Assertion { root: compiled? })
}

fn compile_op(op: &super::Op, num_ops: &mut u32) -> Result<Op, CompilationError> {
    dec_ops(num_ops)?;
    let op = match op {
        super::Op::And { children } => Op::And {
            children: visit_children(children, num_ops)?,
        },
        super::Op::Or { children } => Op::Or {
            children: visit_children(children, num_ops)?,
        },
        super::Op::Not { operand } => Op::Not {
            operand: Box::new(compile_op(operand, num_ops)?),
        },
        super::Op::StatusCodeCheck { value, operator } => Op::StatusCodeCheck {
            value: *value,
            operator: operator.into(),
        },
        super::Op::HeaderCheck {
            key_op,
            key_operand,
            value_op,
            value_operand,
        } => Op::HeaderCheck {
            key_op: key_op.into(),
            value_op: value_op.into(),
            key_operand: key_operand.try_into()?,
            value_operand: value_operand.try_into()?,
        },
        super::Op::JsonPath {
            value,
            operand,
            operator,
        } => Op::JsonPath {
            value: parse_json_path(value, GLOB_COMPLEXITY_LIMIT)?,
            operand: operand.try_into()?,
            operator: operator.into(),
        },
    };

    Ok(op)
}

impl From<ParseIntError> for RuntimeError {
    fn from(value: ParseIntError) -> Self {
        Self::InvalidTypeComparison {
            msg: value.to_string(),
        }
    }
}

impl From<ParseFloatError> for RuntimeError {
    fn from(value: ParseFloatError) -> Self {
        Self::InvalidTypeComparison {
            msg: value.to_string(),
        }
    }
}

impl From<JsonPathError> for RuntimeError {
    fn from(value: JsonPathError) -> Self {
        match value {
            JsonPathError::TookTooLong => RuntimeError::TookTooLong,
            rest => RuntimeError::InvalidJsonPath {
                msg: rest.to_string(),
            },
        }
    }
}

impl From<JsonPathError> for CompilationError {
    fn from(value: JsonPathError) -> Self {
        match value {
            JsonPathError::PestError(error) => {
                let pos = match error.location {
                    pest::error::InputLocation::Pos(pos) => pos,
                    pest::error::InputLocation::Span((start, _)) => start,
                };
                CompilationError::JSONPathParser {
                    msg: error.to_string(),
                    path: error.line().to_owned(),
                    pos,
                }
            }
            JsonPathError::InvalidGlob(glob, reason) => {
                CompilationError::InvalidGlob { glob, msg: reason }
            }
            rest => CompilationError::InvalidJsonPath {
                msg: rest.to_string(),
            },
        }
    }
}

fn visit_children(children: &[super::Op], num_ops: &mut u32) -> Result<Vec<Op>, CompilationError> {
    let mut cs = vec![];
    for c in children.iter() {
        cs.push(compile_op(c, num_ops)?);
    }
    Ok(cs)
}

#[cfg(test)]
mod tests {
    const MAX_ASSERTION_OPS: u32 = 16;
    const ASSERTION_MAX_GAS: u32 = 100;
    const REGION: &str = "default";

    use http::{HeaderMap, HeaderValue};

    use crate::assertions::{
        compiled::{compile, extract_failure_data, CompilationError, EvalPath},
        Assertion, Comparison, GlobPattern, HeaderOperand, JSONPathOperand, Op,
    };

    #[test]
    fn test_status_comparators_and() {
        let assert = Assertion {
            root: Op::And {
                children: vec![
                    Op::StatusCodeCheck {
                        value: 100,
                        operator: Comparison::GreaterThan,
                    },
                    Op::StatusCodeCheck {
                        value: 300,
                        operator: Comparison::LessThan,
                    },
                    Op::StatusCodeCheck {
                        value: 200,
                        operator: Comparison::Equals,
                    },
                    Op::StatusCodeCheck {
                        value: 400,
                        operator: Comparison::NotEqual,
                    },
                ],
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        assert!(
            assert
                .eval(200, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(105, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(95, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(299, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(305, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_status_comparators_or() {
        let assert = Assertion {
            root: Op::Or {
                children: vec![
                    Op::StatusCodeCheck {
                        value: 100,
                        operator: Comparison::LessThan,
                    },
                    Op::StatusCodeCheck {
                        value: 300,
                        operator: Comparison::GreaterThan,
                    },
                    Op::StatusCodeCheck {
                        value: 200,
                        operator: Comparison::Equals,
                    },
                ],
            },
        };
        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        assert!(
            assert
                .eval(200, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(105, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            assert
                .eval(95, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(299, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            assert
                .eval(305, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_booleans_nesting() {
        let assert = Assertion {
            root: Op::Or {
                children: vec![
                    Op::And {
                        children: vec![
                            Op::StatusCodeCheck {
                                value: 0,
                                operator: Comparison::GreaterThan,
                            },
                            Op::StatusCodeCheck {
                                value: 100,
                                operator: Comparison::LessThan,
                            },
                        ],
                    },
                    Op::Not {
                        operand: Op::And {
                            children: vec![Op::StatusCodeCheck {
                                value: 200,
                                operator: Comparison::NotEqual,
                            }],
                        }
                        .into(),
                    },
                    Op::Or {
                        children: vec![
                            Op::StatusCodeCheck {
                                value: 201,
                                operator: Comparison::Equals,
                            },
                            Op::StatusCodeCheck {
                                value: 202,
                                operator: Comparison::Equals,
                            },
                        ],
                    },
                ],
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        assert!(
            !assert
                .eval(0, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            assert
                .eval(15, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(100, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            assert
                .eval(200, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            assert
                .eval(201, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            assert
                .eval(202, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
        assert!(
            !assert
                .eval(203, &HeaderMap::new(), b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_gas_exhaustion() {
        let mut hmap = HeaderMap::new();
        hmap.append("x-header-good", HeaderValue::from_static("0"));
        hmap.append("x-header-good-a", HeaderValue::from_static("1"));
        hmap.append("x-header-good-1", HeaderValue::from_static("2"));
        hmap.append("x-header-good-2", HeaderValue::from_static("0"));
        hmap.append("x-header-good-b", HeaderValue::from_static("1"));
        hmap.append("x-header-good-3", HeaderValue::from_static("2"));

        let body = r#"
  [
    {
      "prop1": "aaaaa"
    },
    {
      "prop1": "aa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aaaaa"
    }
  ]
"#;

        let jsonpath = Op::JsonPath {
            value: "$[?length(@.prop1) > 4]".to_owned(),
            operator: Comparison::Always,
            operand: JSONPathOperand::None,
        };
        let headercheck = Op::HeaderCheck {
            key_op: Comparison::Equals,
            key_operand: HeaderOperand::Glob {
                pattern: GlobPattern {
                    value: "*-good*".to_owned(),
                },
            },
            value_op: Comparison::Always,
            value_operand: HeaderOperand::None,
        };

        let assert = Assertion {
            root: Op::And {
                children: vec![
                    jsonpath.clone(),
                    jsonpath.clone(),
                    jsonpath.clone(),
                    jsonpath.clone(),
                    jsonpath.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                    headercheck.clone(),
                ],
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        let result = assert
            .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
            .err()
            .unwrap();
        assert!(matches!(result, super::RuntimeError::TookTooLong));
    }

    #[test]
    fn test_json_path() {
        let body = r#"
  [
    {
      "prop1": "a"
    },
    {
      "prop1": "aa"
    },
    {
      "prop1": "aaaa"
    },
    {
      "prop1": "aaaaa"
    }
  ]
"#;
        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?length(@.prop1) > 4].prop1".to_owned(),
                operand: JSONPathOperand::Glob {
                    pattern: GlobPattern {
                        value: "a*a".into(),
                    },
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?length(@.prop1) > 10].prop1".to_owned(),
                operand: JSONPathOperand::Glob {
                    pattern: GlobPattern {
                        value: "a*a".into(),
                    },
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            !assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"a[a-z]*\")]".to_owned(),
                operand: JSONPathOperand::None,
                operator: Comparison::Always,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        assert!(
            assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"A[A-Z]*\")].prop1".to_owned(),
                operand: JSONPathOperand::None,
                operator: Comparison::Always,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            !assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_json_path_object() {
        let body = r#"
    {
      "prop1": {
        "id" : "123"
      }
    }
"#;
        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$.prop1.id".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "123".into(),
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let assert = Assertion {
            root: Op::JsonPath {
                value: "$.prop1.id".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "124".into(),
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            !assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_json_path_coerce() {
        let body = r#"
    {
      "prop1": {
        "id1" : 123,
        "id2": 123.1,
        "id3": "123"
      }
    }
"#;
        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$.prop1.id1".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "123".into(),
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$.prop1.id2".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "123.1".into(),
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$.prop1.id3".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "123".into(),
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$.prop1.id".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "onetwothree".into(),
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            !assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_json_path_arrays() {
        let body = r#"
    [
      {
      "prop": {
        "id": "123"
      }
      },
      {
      "prop": {
        "id": "123"
      }
      }
    ]
"#;
        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[*].prop.id".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "123".into(),
                },
                operator: Comparison::Equals,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let body = r#"
    [
      {
      "prop": {
        "id": "123"
      }
      },
      {
      "prop": {
        "id": "124"
      }
      }
    ]
"#;
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[*].prop.id".to_owned(),
                operand: JSONPathOperand::Literal {
                    value: "124".into(),
                },
                operator: Comparison::NotEqual,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();

        assert!(
            !assert
                .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_header_regex() {
        let mut hmap = HeaderMap::new();
        hmap.append("x-header-good", HeaderValue::from_static("0"));
        hmap.append("x-header-good-a", HeaderValue::from_static("1"));
        hmap.append("x-header-good-1", HeaderValue::from_static("2"));

        let assert = Assertion {
            root: Op::HeaderCheck {
                key_op: Comparison::Equals,
                key_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: r"*-good-[0-9]".into(),
                    },
                },
                value_op: Comparison::Always,
                value_operand: HeaderOperand::None,
            },
        };
        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        assert!(
            assert
                .eval(200, &hmap, b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let assert = Assertion {
            root: Op::Or {
                children: vec![Op::HeaderCheck {
                    key_op: Comparison::Equals,
                    key_operand: HeaderOperand::Glob {
                        pattern: GlobPattern {
                            value: r"*-good-[0-9]".into(),
                        },
                    },
                    value_op: Comparison::Equals,
                    value_operand: HeaderOperand::Literal { value: "2".into() },
                }],
            },
        };
        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        assert!(
            assert
                .eval(200, &hmap, b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );

        let assert = Assertion {
            root: Op::HeaderCheck {
                key_op: Comparison::Equals,
                key_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: r"*-good-[0-9]".into(),
                    },
                },

                value_op: Comparison::Equals,
                value_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: r"[0-9][0-9]".into(),
                    },
                },
            },
        };
        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        assert!(
            !assert
                .eval(200, &hmap, b"", ASSERTION_MAX_GAS, REGION)
                .unwrap()
                .result
        );
    }

    #[test]
    fn test_broken_jsonpath_glob() {
        let assert = Assertion {
            root: Op::JsonPath {
                value: "$[?glob(@.prop1, \"'a']\")]".to_owned(),
                operand: JSONPathOperand::None,
                operator: Comparison::Always,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap_err();
        assert!(matches!(
            assert,
            CompilationError::InvalidGlob { msg: _, glob: _ }
        ));
    }

    #[test]
    fn test_broken_header_glob() {
        let assert = Assertion {
            root: Op::HeaderCheck {
                key_op: Comparison::Equals,
                key_operand: HeaderOperand::Glob {
                    pattern: GlobPattern {
                        value: "'a']".into(),
                    },
                },
                value_op: Comparison::Always,
                value_operand: HeaderOperand::None,
            },
        };

        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap_err();
        assert!(matches!(
            assert,
            CompilationError::InvalidGlob { msg: _, glob: _ }
        ));
    }
    #[test]
    fn test_failing_path_or() {
        let mut hmap = HeaderMap::new();
        hmap.append("4", HeaderValue::from_static("*"));
        hmap.append("2", HeaderValue::from_static("*"));
        hmap.append("5", HeaderValue::from_static("*"));
        let assert = Assertion {
            root: Op::Not {
                operand: Op::Or {
                    children: vec![
                        Op::HeaderCheck {
                            key_op: Comparison::Equals,
                            key_operand: HeaderOperand::Literal { value: "1".into() },
                            value_op: Comparison::Always,
                            value_operand: HeaderOperand::None,
                        },
                        Op::HeaderCheck {
                            key_op: Comparison::Equals,
                            key_operand: HeaderOperand::Literal { value: "2".into() },
                            value_op: Comparison::Always,
                            value_operand: HeaderOperand::None,
                        },
                        Op::HeaderCheck {
                            key_op: Comparison::Equals,
                            key_operand: HeaderOperand::Literal { value: "3".into() },
                            value_op: Comparison::Always,
                            value_operand: HeaderOperand::None,
                        },
                    ],
                }
                .into(),
            },
        };
        let c_assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        let eval = c_assert
            .eval(200, &hmap, b"", ASSERTION_MAX_GAS, REGION)
            .unwrap();
        assert!(!eval.result);
        assert_eq!(
            eval.reason_path,
            EvalPath::ChildIndex {
                index: 0,
                child: EvalPath::ChildIndex {
                    index: 1,
                    child: EvalPath::Leaf.into()
                }
                .into()
            }
        );

        let extracted_failure = extract_failure_data(&eval.reason_path, &assert.root);
        match extracted_failure {
            Op::Not { operand } => match *operand {
                Op::Or { children } => assert_eq!(
                    children[0],
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "2".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    }
                ),
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn test_failing_path_and() {
        let mut hmap = HeaderMap::new();
        hmap.append("1", HeaderValue::from_static("*"));
        hmap.append("2", HeaderValue::from_static("*"));
        hmap.append("3", HeaderValue::from_static("*"));
        let assert = Assertion {
            root: Op::And {
                children: vec![
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "1".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    },
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "2".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    },
                    Op::HeaderCheck {
                        key_op: Comparison::Equals,
                        key_operand: HeaderOperand::Literal { value: "4".into() },
                        value_op: Comparison::Always,
                        value_operand: HeaderOperand::None,
                    },
                ],
            },
        };
        let c_assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        let eval = c_assert
            .eval(200, &hmap, b"", ASSERTION_MAX_GAS, REGION)
            .unwrap();
        assert!(!eval.result);
        assert_eq!(
            eval.reason_path,
            EvalPath::ChildIndex {
                index: 2,
                child: EvalPath::Leaf.into()
            }
        );

        let extracted_failure = extract_failure_data(&eval.reason_path, &assert.root);
        match extracted_failure {
            Op::And { children } => assert_eq!(
                children[0],
                Op::HeaderCheck {
                    key_op: Comparison::Equals,
                    key_operand: HeaderOperand::Literal { value: "4".into() },
                    value_op: Comparison::Always,
                    value_operand: HeaderOperand::None,
                },
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn test_failing_path_nesting() {
        let mut hmap = HeaderMap::new();
        hmap.append("1", HeaderValue::from_static("*"));
        hmap.append("2", HeaderValue::from_static("*"));
        hmap.append("3", HeaderValue::from_static("*"));
        let assert = Assertion {
            root: Op::Not {
                operand: Op::Or {
                    children: vec![
                        Op::And {
                            children: vec![
                                Op::HeaderCheck {
                                    key_op: Comparison::Equals,
                                    key_operand: HeaderOperand::Literal { value: "1".into() },
                                    value_op: Comparison::Always,
                                    value_operand: HeaderOperand::None,
                                },
                                Op::HeaderCheck {
                                    key_op: Comparison::NotEqual,
                                    key_operand: HeaderOperand::Literal {
                                        value: "123".into(),
                                    },
                                    value_op: Comparison::Always,
                                    value_operand: HeaderOperand::None,
                                },
                            ],
                        },
                        Op::Or {
                            children: vec![
                                Op::Not {
                                    operand: Op::StatusCodeCheck {
                                        value: 200,
                                        operator: Comparison::Equals,
                                    }
                                    .into(),
                                },
                                Op::StatusCodeCheck {
                                    value: 201,
                                    operator: Comparison::Equals,
                                },
                            ],
                        },
                    ],
                }
                .into(),
            },
        };
        let c_assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        let eval = c_assert
            .eval(200, &hmap, b"", ASSERTION_MAX_GAS, REGION)
            .unwrap();
        assert!(!eval.result);
        assert_eq!(
            eval.reason_path,
            EvalPath::ChildIndex {
                index: 0,
                child: EvalPath::ChildIndex {
                    index: 0,
                    child: EvalPath::AllChildren.into()
                }
                .into()
            }
        );

        let extracted_failure = extract_failure_data(&eval.reason_path, &assert.root);
        match extracted_failure {
            Op::Not { operand } => match *operand {
                Op::Or { children } => match &children[0] {
                    Op::And { children } => assert_eq!(
                        *children,
                        vec![
                            Op::HeaderCheck {
                                key_op: Comparison::Equals,
                                key_operand: HeaderOperand::Literal { value: "1".into() },
                                value_op: Comparison::Always,
                                value_operand: HeaderOperand::None,
                            },
                            Op::HeaderCheck {
                                key_op: Comparison::NotEqual,
                                key_operand: HeaderOperand::Literal {
                                    value: "123".into(),
                                },
                                value_op: Comparison::Always,
                                value_operand: HeaderOperand::None,
                            },
                        ],
                    ),
                    _ => panic!(),
                },
                _ => panic!(),
            },
            _ => panic!(),
        }
    }
    #[test]
    fn test_comparisons() {
        let body = r#"
    {
      "prop": {
        "string": "123",
        "int": 123,
        "float": 123.0
      }
    }
"#;

        // Equals
        assert!(run_single_jsonpath_assert(
            "$.prop.string",
            "123",
            Comparison::Equals,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.int",
            "123",
            Comparison::Equals,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.float",
            "123.0",
            Comparison::Equals,
            body
        ));
        // Equals (negation)
        assert!(!run_single_jsonpath_assert(
            "$.prop.string",
            "124",
            Comparison::Equals,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.int",
            "124",
            Comparison::Equals,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.float",
            "123.1",
            Comparison::Equals,
            body
        ));

        // NotEquals
        assert!(run_single_jsonpath_assert(
            "$.prop.string",
            "123a",
            Comparison::NotEqual,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.int",
            "124",
            Comparison::NotEqual,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.float",
            "123.1",
            Comparison::NotEqual,
            body
        ));
        // NotEquals (negation)
        assert!(!run_single_jsonpath_assert(
            "$.prop.string",
            "123",
            Comparison::NotEqual,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.int",
            "123",
            Comparison::NotEqual,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.float",
            "123.0",
            Comparison::NotEqual,
            body
        ));

        // GreaterThan
        assert!(run_single_jsonpath_assert(
            "$.prop.string",
            "023",
            Comparison::GreaterThan,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.int",
            "122",
            Comparison::GreaterThan,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.float",
            "122.9",
            Comparison::GreaterThan,
            body
        ));
        // GreaterThan (negation)
        assert!(!run_single_jsonpath_assert(
            "$.prop.string",
            "123",
            Comparison::GreaterThan,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.int",
            "123",
            Comparison::GreaterThan,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.float",
            "123.0",
            Comparison::GreaterThan,
            body
        ));

        // LessThan
        assert!(run_single_jsonpath_assert(
            "$.prop.string",
            "223",
            Comparison::LessThan,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.int",
            "124",
            Comparison::LessThan,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.float",
            "123.1",
            Comparison::LessThan,
            body
        ));
        // LessThan (negation)
        assert!(!run_single_jsonpath_assert(
            "$.prop.string",
            "123",
            Comparison::LessThan,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.int",
            "123",
            Comparison::LessThan,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.float",
            "123.0",
            Comparison::LessThan,
            body
        ));

        // Always
        assert!(run_single_jsonpath_assert(
            "$.prop.string",
            "",
            Comparison::Always,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.int",
            "",
            Comparison::Always,
            body
        ));
        assert!(run_single_jsonpath_assert(
            "$.prop.float",
            "",
            Comparison::Always,
            body
        ));

        // Never
        assert!(!run_single_jsonpath_assert(
            "$.prop.string",
            "",
            Comparison::Never,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.int",
            "",
            Comparison::Never,
            body
        ));
        assert!(!run_single_jsonpath_assert(
            "$.prop.float",
            "",
            Comparison::Never,
            body
        ));
    }

    fn run_single_jsonpath_assert(
        path: &str,
        operand: &str,
        operator: Comparison,
        body: &str,
    ) -> bool {
        let hmap = HeaderMap::new();
        let assert = Assertion {
            root: Op::JsonPath {
                value: path.to_owned(),
                operand: JSONPathOperand::Literal {
                    value: operand.into(),
                },
                operator,
            },
        };
        let assert = compile(&assert, MAX_ASSERTION_OPS, REGION).unwrap();
        let eval = assert
            .eval(200, &hmap, body.as_bytes(), ASSERTION_MAX_GAS, REGION)
            .unwrap();
        eval.result
    }
}
