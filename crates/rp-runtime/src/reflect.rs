//! `std/reflect`：运行时类型信息与动态派发。
//!
//! 静态类型仍是首选；反射用于日志、序列化、插件桥接等确实需要在运行期
//! 读取结构体字段或按名字调用方法的场景。

use crate::rt::{Flow, FlowResult, Runtime};
use crate::value::{format_value, Value};

impl Runtime {
    pub(crate) fn reflect_module(
        &mut self,
        name: &str,
        args: Vec<Value>,
        line: u32,
    ) -> FlowResult<Value> {
        match name {
            "type_name" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                Ok(Value::string(type_name_of(&value)))
            }
            "kind" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                Ok(Value::string(kind_of(&value)))
            }
            "fields" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                Ok(Value::vec(
                    field_names(&value)
                        .into_iter()
                        .map(Value::string)
                        .collect(),
                ))
            }
            "has" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                let key = self.string_arg(&args, 1, name, line)?;
                Ok(Value::Bool(read_field(&value, &key).is_some()))
            }
            "get" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                let key = self.string_arg(&args, 1, name, line)?;
                Ok(match read_field(&value, &key) {
                    Some(field) => Value::Some(Box::new(field)),
                    None => Value::None,
                })
            }
            "variant" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                Ok(match &value {
                    Value::Ary(ary) => ary
                        .borrow()
                        .get("#variant")
                        .and_then(|variant| variant.as_str())
                        .map(|variant| Value::Some(Box::new(Value::string(variant))))
                        .unwrap_or(Value::None),
                    _ => Value::None,
                })
            }
            "is" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                let expected = self.string_arg(&args, 1, name, line)?;
                Ok(Value::Bool(
                    type_name_of(&value) == expected || kind_of(&value) == expected,
                ))
            }
            // 按名字调用方法：参数是 vec<any>，返回 result<any, string>。
            "invoke" => {
                let value = args.first().cloned().unwrap_or(Value::Null);
                let method = self.string_arg(&args, 1, name, line)?;
                let values = match args.get(2) {
                    Some(Value::Vec(items)) => items.borrow().clone(),
                    None | Some(Value::Null) => Vec::new(),
                    _ => return self.error("reflect.invoke expects vec<any> arguments", line),
                };
                match self.call_method(value, &method, values, line) {
                    Ok(result) => Ok(Value::Ok(Box::new(result))),
                    Err(Flow::Error(error)) => Ok(Value::Err(Box::new(Value::string(error.message)))),
                    Err(Flow::Propagate(value)) => Ok(Value::Err(Box::new(Value::string(
                        format!("uncaught error: {}", format_value(&value)),
                    )))),
                    Err(Flow::Return(value)) => Ok(Value::Ok(Box::new(value))),
                    Err(Flow::Break) | Err(Flow::Continue) => Ok(Value::Err(Box::new(
                        Value::string("break/continue outside loop"),
                    ))),
                }
            }
            _ => self.error(format!("std/reflect has no function '{}'", name), line),
        }
    }
}

/// 具体类型名：结构体/枚举返回声明的名字，其余返回内建类型名。
fn type_name_of(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::None => "none".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Int(_) => "int".to_string(),
        Value::UInt(_) => "uint".to_string(),
        Value::Float(_) => "float".to_string(),
        Value::Str(_) => "string".to_string(),
        Value::Char(_) => "char".to_string(),
        Value::Some(_) => "option".to_string(),
        Value::Ok(_) | Value::Err(_) => "result".to_string(),
        Value::Vec(_) => "vec".to_string(),
        Value::Map(_) => "map".to_string(),
        Value::Func(_) => "func".to_string(),
        Value::Task(_) => "task".to_string(),
        Value::Ary(ary) => ary.borrow().class.clone(),
    }
}

/// 归类名：结构体与枚举统一为 `struct`。
fn kind_of(value: &Value) -> String {
    match value {
        Value::Ary(_) => "struct".to_string(),
        other => type_name_of(other),
    }
}

fn field_names(value: &Value) -> Vec<String> {
    match value {
        Value::Ary(ary) => ary
            .borrow()
            .fields
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| name != "#variant")
            .collect(),
        Value::Map(map) => map
            .borrow()
            .entries
            .iter()
            .map(|(key, _)| key.as_str().unwrap_or_else(|| format_value(key)))
            .collect(),
        Value::Vec(items) => (0..items.borrow().len()).map(|index| index.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn read_field(value: &Value, key: &str) -> Option<Value> {
    match value {
        Value::Ary(ary) => ary.borrow().get(key),
        Value::Map(map) => map
            .borrow()
            .entries
            .iter()
            .find(|(candidate, _)| candidate.as_str().as_deref() == Some(key))
            .map(|(_, value)| value.clone()),
        Value::Vec(items) => {
            let index: usize = key.parse().ok()?;
            items.borrow().get(index).cloned()
        }
        _ => None,
    }
}
