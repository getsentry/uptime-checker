use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct Assertion {
    root: Op,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "cmp")]
#[serde(rename_all = "snake_case")]

enum Comparison {
    LessThan,
    GreaterThan,
    Equal,
    NotEqual,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "header_op")]
#[serde(rename_all = "snake_case")]
enum HeaderOperand {
    Literal { value: String },
    Regex { value: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "header_cmp")]
#[serde(rename_all = "snake_case")]
enum HeaderComparison {
    Always,
    Never,
    Equals { test_value: HeaderOperand },
    NotEquals { test_value: HeaderOperand },
    LessThan { test_value: String },
    GreaterThan { test_value: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op")]
#[serde(rename_all = "snake_case")]
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
    // JsonPath {
    //     value: JsonPathThingy,
    // },
    // XmlPath {
    //     value: XmlPathThingy,
    // },
    HeaderCheck {
        key: HeaderComparison,
        value: HeaderComparison,
    },
}

pub mod compiled {
    use http::HeaderValue;
    use regex::{Regex, RegexBuilder};
    use std::str::FromStr;

    // Don't make this any lower--there are some really innoccuous looking regexes (\d, \w) that
    // actually expand to fairly large sizes, owing to large numbers of unicode characters.
    const REGEX_SIZE_LIMIT: usize = 65536;

    #[derive(thiserror::Error, Debug)]
    pub enum Error {
        #[error("Invalid regex syntax")]
        InvalidRegex(String),

        #[error("Regex is too complicated")]
        RegexTooBig(usize),
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Assertion {
        root: Op,
    }

    impl Assertion {
        pub fn eval(
            &self,
            status_code: u16,
            headers: &hyper::header::HeaderMap<HeaderValue>,
            body: &str,
        ) -> bool {
            self.root.eval(status_code, headers, body)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Comparison {
        LessThan,
        GreaterThan,
        Equal,
        NotEqual,
    }

    impl From<&super::Comparison> for Comparison {
        fn from(value: &super::Comparison) -> Self {
            match *value {
                super::Comparison::LessThan => Comparison::LessThan,
                super::Comparison::GreaterThan => Comparison::GreaterThan,
                super::Comparison::Equal => Comparison::Equal,
                super::Comparison::NotEqual => Comparison::NotEqual,
            }
        }
    }

    #[derive(Debug, Clone)]
    enum HeaderOperand {
        Literal { value: Value },
        Regex { value: Regex },
    }

    impl Eq for HeaderOperand {}

    impl PartialEq for HeaderOperand {
        fn eq(&self, other: &Self) -> bool {
            match (self, other) {
                (Self::Literal { value: l_value }, Self::Literal { value: r_value }) => {
                    l_value == r_value
                }
                (Self::Regex { value: l_value }, Self::Regex { value: r_value }) => {
                    l_value.as_str() == r_value.as_str()
                }
                _ => false,
            }
        }
    }

    impl TryFrom<&super::HeaderOperand> for HeaderOperand {
        type Error = Error;
        fn try_from(value: &super::HeaderOperand) -> Result<Self, Self::Error> {
            let v = match value {
                crate::types::assertion::HeaderOperand::Literal { value } => {
                    HeaderOperand::Literal {
                        value: value.into(),
                    }
                }
                crate::types::assertion::HeaderOperand::Regex { value } => {
                    let r = RegexBuilder::new(value)
                        .size_limit(REGEX_SIZE_LIMIT)
                        .build();

                    match r {
                        Ok(value) => HeaderOperand::Regex { value },
                        Err(e) => {
                            return Err(match e {
                                regex::Error::Syntax(s) => Error::InvalidRegex(s),
                                regex::Error::CompiledTooBig(s) => Error::RegexTooBig(s),
                                _ => panic!("unhandled new error"),
                            })
                        }
                    }
                }
            };

            Ok(v)
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Value {
        I64(i64),
        F64(f64),
        String(String),
    }

    impl Eq for Value {}

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum HeaderComparison {
        Always,
        Never,
        Equals { test_value: HeaderOperand },
        NotEquals { test_value: HeaderOperand },
        LessThan { test_value: Value },
        GreaterThan { test_value: Value },
    }

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

    impl TryFrom<&super::HeaderComparison> for HeaderComparison {
        type Error = Error;
        fn try_from(value: &super::HeaderComparison) -> Result<Self, Self::Error> {
            let v = match value {
                super::HeaderComparison::Always => HeaderComparison::Always,
                super::HeaderComparison::Never => HeaderComparison::Never,
                super::HeaderComparison::Equals { test_value } => HeaderComparison::Equals {
                    test_value: test_value.try_into()?,
                },
                super::HeaderComparison::NotEquals { test_value } => HeaderComparison::NotEquals {
                    test_value: test_value.try_into()?,
                },
                super::HeaderComparison::LessThan { test_value } => HeaderComparison::LessThan {
                    test_value: test_value.into(),
                },
                super::HeaderComparison::GreaterThan { test_value } => {
                    HeaderComparison::GreaterThan {
                        test_value: test_value.into(),
                    }
                }
            };

            Ok(v)
        }
    }

    fn cmp_eq<T>(header_value: &str, value: &T) -> bool
    where
        T: FromStr + PartialEq,
    {
        header_value
            .parse::<T>()
            .and_then(|v| Ok(v == *value))
            .unwrap_or(false)
    }

    fn cmp_lt<T>(header_value: &str, value: &T) -> bool
    where
        T: FromStr + PartialOrd,
    {
        header_value
            .parse::<T>()
            .and_then(|v| Ok(v < *value))
            .unwrap_or(false)
    }

    fn cmp_gt<T>(header_value: &str, value: &T) -> bool
    where
        T: FromStr + PartialOrd,
    {
        header_value
            .parse::<T>()
            .and_then(|v| Ok(v > *value))
            .unwrap_or(false)
    }

    fn cmp_eq_header(header_value: &str, test_value: &HeaderOperand) -> bool {
        match test_value {
            HeaderOperand::Literal { value } => match value {
                Value::I64(value) => cmp_eq(header_value, value),
                Value::F64(value) => cmp_eq(header_value, value),
                Value::String(value) => cmp_eq(header_value, value),
            },
            HeaderOperand::Regex { value } => value.is_match(header_value),
        }
    }

    impl HeaderComparison {
        fn eval(&self, header_value: &str) -> bool {
            match self {
                HeaderComparison::Always => true,
                HeaderComparison::Never => false,
                HeaderComparison::Equals { test_value } => cmp_eq_header(header_value, test_value),
                HeaderComparison::NotEquals { test_value } => {
                    !cmp_eq_header(header_value, test_value)
                }

                HeaderComparison::LessThan { test_value } => match test_value {
                    Value::I64(value) => cmp_lt(header_value, value),
                    Value::F64(value) => cmp_lt(header_value, value),
                    Value::String(value) => cmp_lt(header_value, value),
                },
                HeaderComparison::GreaterThan { test_value } => match test_value {
                    Value::I64(value) => cmp_gt(header_value, value),
                    Value::F64(value) => cmp_gt(header_value, value),
                    Value::String(value) => cmp_gt(header_value, value),
                },
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
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
        // JsonPath {
        //     value: JsonPathThingy,
        // },
        // XmlPath {
        //     value: XmlPathThingy,
        // },
        HeaderCheck {
            key: HeaderComparison,
            value: HeaderComparison,
        },
    }

    impl Op {
        fn eval(
            &self,
            status_code: u16,
            headers: &hyper::header::HeaderMap<HeaderValue>,
            body: &str,
        ) -> bool {
            match self {
                Op::And { children } => children
                    .iter()
                    .all(|op| op.eval(status_code, headers, body)),
                Op::Or { children } => children
                    .iter()
                    .any(|op| op.eval(status_code, headers, body)),
                Op::Not { operand } => !operand.eval(status_code, headers, body),
                Op::StatusCodeCheck { value, operator } => match operator {
                    Comparison::LessThan => status_code < *value,
                    Comparison::GreaterThan => status_code > *value,
                    Comparison::Equal => *value == status_code,
                    Comparison::NotEqual => *value != status_code,
                },
                Op::HeaderCheck { key, value } => {
                    // Find any header that passes key.  Then, see if it's value passes value.
                    headers
                        .iter()
                        .any(|(k, v)| key.eval(k.as_str()) && value.eval(v.to_str().unwrap()))
                }
            }
        }
    }

    pub fn compile(assertion: &super::Assertion) -> Result<Assertion, Error> {
        Ok(Assertion {
            root: compile_op(&assertion.root)?,
        })
    }

    fn compile_op(op: &super::Op) -> Result<Op, Error> {
        let op = match op {
            super::Op::And { children } => Op::And {
                children: fun_name(children)?,
            },
            super::Op::Or { children } => Op::Or {
                children: fun_name(children)?,
            },
            super::Op::Not { operand } => Op::Not {
                operand: Box::new(compile_op(&**operand)?),
            },
            super::Op::StatusCodeCheck { value, operator } => Op::StatusCodeCheck {
                value: *value,
                operator: operator.into(),
            },
            super::Op::HeaderCheck { key, value } => Op::HeaderCheck {
                key: key.try_into()?,
                value: value.try_into()?,
            },
        };

        Ok(op)
    }

    fn fun_name(children: &Vec<super::Op>) -> Result<Vec<Op>, Error> {
        let mut cs = vec![];
        for c in children.iter() {
            cs.push(compile_op(c)?);
        }
        Ok(cs)
    }
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};

    use crate::types::assertion::{
        compiled::{self, compile},
        Assertion, Comparison, HeaderComparison, HeaderOperand,
    };

    use super::Op;

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
                        operator: Comparison::Equal,
                    },
                    Op::StatusCodeCheck {
                        value: 400,
                        operator: Comparison::NotEqual,
                    },
                ],
            },
        };
        let assert = compile(&assert).unwrap();
        assert_eq!(assert.eval(200, &HeaderMap::new(), ""), true);
        assert_eq!(assert.eval(105, &HeaderMap::new(), ""), false);
        assert_eq!(assert.eval(95, &HeaderMap::new(), ""), false);
        assert_eq!(assert.eval(299, &HeaderMap::new(), ""), false);
        assert_eq!(assert.eval(305, &HeaderMap::new(), ""), false);
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
                        operator: Comparison::Equal,
                    },
                ],
            },
        };
        let assert = compile(&assert).unwrap();
        assert_eq!(assert.eval(200, &HeaderMap::new(), ""), true);
        assert_eq!(assert.eval(105, &HeaderMap::new(), ""), false);
        assert_eq!(assert.eval(95, &HeaderMap::new(), ""), true);
        assert_eq!(assert.eval(299, &HeaderMap::new(), ""), false);
        assert_eq!(assert.eval(305, &HeaderMap::new(), ""), true);
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
                                operator: Comparison::Equal,
                            },
                            Op::StatusCodeCheck {
                                value: 202,
                                operator: Comparison::Equal,
                            },
                        ],
                    },
                ],
            },
        };

        let assert = compile(&assert).unwrap();
        assert_eq!(assert.eval(0, &HeaderMap::new(), ""), false);
        assert_eq!(assert.eval(15, &HeaderMap::new(), ""), true);
        assert_eq!(assert.eval(100, &HeaderMap::new(), ""), false);
        assert_eq!(assert.eval(200, &HeaderMap::new(), ""), true);
        assert_eq!(assert.eval(201, &HeaderMap::new(), ""), true);
        assert_eq!(assert.eval(202, &HeaderMap::new(), ""), true);
        assert_eq!(assert.eval(203, &HeaderMap::new(), ""), false);
    }

    #[test]
    fn test_header_regex() {
        let mut hmap = HeaderMap::new();
        hmap.append("x-header-good", HeaderValue::from_static("0"));
        hmap.append("x-header-good-a", HeaderValue::from_static("1"));
        hmap.append("x-header-good-1", HeaderValue::from_static("2"));

        let assert = Assertion {
            root: Op::HeaderCheck {
                key: HeaderComparison::Equals {
                    test_value: HeaderOperand::Regex {
                        value: r".*-good-\d".into(),
                    },
                },
                value: HeaderComparison::Always,
            },
        };
        let assert = compile(&assert).unwrap();
        assert_eq!(assert.eval(200, &hmap, ""), true);

        let assert = Assertion {
            root: Op::Or {
                children: vec![Op::HeaderCheck {
                    key: HeaderComparison::Equals {
                        test_value: HeaderOperand::Regex {
                            value: r".*-good-\d".into(),
                        },
                    },
                    value: HeaderComparison::Equals {
                        test_value: HeaderOperand::Literal { value: "2".into() },
                    },
                }],
            },
        };
        let assert = compile(&assert).unwrap();
        assert_eq!(assert.eval(200, &hmap, ""), true);

        let assert = Assertion {
            root: Op::HeaderCheck {
                key: HeaderComparison::Equals {
                    test_value: HeaderOperand::Regex {
                        value: r".*-good-\d".into(),
                    },
                },
                value: HeaderComparison::Equals {
                    test_value: HeaderOperand::Regex {
                        value: r"\d\d".into(),
                    },
                },
            },
        };
        let assert = compile(&assert).unwrap();
        assert_eq!(assert.eval(200, &hmap, ""), false);
    }

    #[test]
    fn test_serialize_roundtrip() {
        let s = r#"{
  "root": {
    "op": "or",
    "children": [
      {
        "op": "and",
        "children": [
          {
            "op": "header_check",
            "key": {
              "header_cmp": "equals",
              "test_value": {
                "header_op": "regex",
                "value": "x-header-\\w+-\\d+"
              }
            },
            "value": {
              "header_cmp": "not_equals",
              "test_value": {
                "header_op": "literal",
                "value": "2"
              }
            }
          },
          {
            "op": "status_code_check",
            "value": 0,
            "operator": {
              "cmp": "greater_than"
            }
          },
          {
            "op": "status_code_check",
            "value": 100,
            "operator": {
              "cmp": "less_than"
            }
          }
        ]
      },
      {
        "op": "not",
        "operand": {
          "op": "and",
          "children": [
            {
              "op": "status_code_check",
              "value": 200,
              "operator": {
                "cmp": "not_equal"
              }
            }
          ]
        }
      },
      {
        "op": "or",
        "children": [
          {
            "op": "status_code_check",
            "value": 201,
            "operator": {
              "cmp": "equal"
            }
          },
          {
            "op": "status_code_check",
            "value": 202,
            "operator": {
              "cmp": "equal"
            }
          }
        ]
      }
    ]
  }
}"#;

        let j: Assertion = serde_json::from_str(s).unwrap();
        let s2 = serde_json::to_string_pretty(&j).unwrap();

        assert_eq!(s, s2);
    }
}
