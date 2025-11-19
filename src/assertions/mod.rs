pub mod compiled;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct Assertion {
    pub(crate) root: Op,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "cmp", rename_all = "snake_case")]
pub(crate) enum Comparison {
    LessThan,
    GreaterThan,
    Equal,
    NotEqual,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) struct GlobPattern {
    pub(crate) value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "header_op", rename_all = "snake_case")]
pub(crate) enum HeaderOperand {
    Literal { value: String },
    Glob { pattern: GlobPattern },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "header_cmp", rename_all = "snake_case")]
pub(crate) enum HeaderComparison {
    Always,
    Never,
    Equals { test_value: HeaderOperand },
    NotEquals { test_value: HeaderOperand },
    LessThan { test_value: String },
    GreaterThan { test_value: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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
    },
    HeaderCheck {
        key: HeaderComparison,
        value: HeaderComparison,
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
            "value": "$[?length(@.prop1) > 4]"
          },
          {
            "op": "header_check",
            "key": {
              "header_cmp": "equals",
              "test_value": {
                "header_op": "glob",
                "pattern": {
                  "value": "x-header-[a-zA-Z]+-[1-9][0-9]*"
                }
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
