pub mod cache;
pub mod compiled;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub struct Assertion {
    pub(crate) root: Op,
}

impl Assertion {
    fn requires_body_impl(op: &Op) -> bool {
        match op {
            Op::And { children } => children.iter().any(Assertion::requires_body_impl),
            Op::Or { children } => children.iter().any(Assertion::requires_body_impl),
            Op::Not { operand } => Assertion::requires_body_impl(&*operand),
            Op::StatusCodeCheck {
                value: _,
                operator: _,
            } => false,
            Op::JsonPath {
                value: _,
                operator: _,
                operand: _,
            } => true,
            Op::HeaderCheck {
                key_op: _,
                key_operand: _,
                value_op: _,
                value_operand: _,
            } => false,
        }
    }

    pub fn requires_body(&self) -> bool {
        Assertion::requires_body_impl(&self.root)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(tag = "cmp", rename_all = "snake_case")]
#[derive(Default)]
pub(crate) enum Comparison {
    #[default]
    Always,
    Never,
    LessThan,
    GreaterThan,
    Equals,
    NotEqual,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) struct GlobPattern {
    pub(crate) value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(tag = "header_op", rename_all = "snake_case")]
pub(crate) enum HeaderOperand {
    None,
    Literal { value: String },
    Glob { pattern: GlobPattern },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(tag = "jsonpath_op", rename_all = "snake_case")]
#[derive(Default)]
pub(crate) enum JSONPathOperand {
    #[default]
    None,
    Literal {
        value: String,
    },
    Glob {
        pattern: GlobPattern,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Op {
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
        value: String,

        // These values were added later, so provide sensible defaults for
        // backwards compatibility.
        #[serde(default)]
        operator: Comparison,
        #[serde(default)]
        operand: JSONPathOperand,
    },
    HeaderCheck {
        key_op: Comparison,
        key_operand: HeaderOperand,
        value_op: Comparison,
        value_operand: HeaderOperand,
    },
}

#[cfg(test)]
mod tests {
    use crate::assertions::Assertion;

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
            "op": "json_path",
            "value": "$[?length(@.prop1) > 4]",
            "operator": {
              "cmp": "always"
            },
            "operand": {
              "jsonpath_op": "none"
            }
          },
          {
            "op": "header_check",
            "key_op": {
              "cmp": "equals"
            },
            "key_operand": {
              "header_op": "glob",
              "pattern": {
                "value": "x-header-[a-zA-Z]+-[1-9][0-9]*"
              }
            },
            "value_op": {
              "cmp": "not_equal"
            },
            "value_operand": {
              "header_op": "literal",
              "value": "2"
            }
          },
          {
            "op": "header_check",
            "key_op": {
              "cmp": "equals"
            },
            "key_operand": {
              "header_op": "glob",
              "pattern": {
                "value": "x-header-[a-zA-Z]+-[1-9][0-9]*"
              }
            },
            "value_op": {
              "cmp": "always"
            },
            "value_operand": {
              "header_op": "none"
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
              "cmp": "equals"
            }
          },
          {
            "op": "status_code_check",
            "value": 202,
            "operator": {
              "cmp": "equals"
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
