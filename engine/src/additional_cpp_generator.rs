// Copyright 2020 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{
    function_wrapper::{FunctionWrapper, FunctionWrapperPayload},
    type_database::TypeDatabase,
};
use itertools::Itertools;
use std::collections::HashSet;

/// Instructions for new C++ which we need to generate.
pub(crate) enum AdditionalNeed {
    MakeStringConstructor,
    FunctionWrapper(Box<FunctionWrapper>),
}

#[derive(Ord, PartialOrd, Eq, PartialEq, Clone, Hash)]
struct Header {
    name: &'static str,
    system: bool,
}

impl Header {
    fn system(name: &'static str) -> Self {
        Header { name, system: true }
    }

    fn user(name: &'static str) -> Self {
        Header {
            name,
            system: false,
        }
    }

    fn include_stmt(&self) -> String {
        if self.system {
            format!("#include <{}>", self.name)
        } else {
            format!("#include \"{}\"", self.name)
        }
    }
}

struct AdditionalFunction {
    declaration: String,
    definition: String,
    headers: Vec<Header>,
}

/// Details of additional generated C++.
pub(crate) struct AdditionalCpp {
    pub(crate) declarations: String,
    pub(crate) definitions: String,
}

/// Generates additional C++ glue functions needed by autocxx.
/// In some ways it would be preferable to be able to pass snippets
/// of C++ through to `cxx` for inclusion in the C++ file which it
/// generates, and perhaps we'll explore that in future. But for now,
/// autocxx generates its own _additional_ C++ files which therefore
/// need to be built and included in linking procedures.
pub(crate) struct AdditionalCppGenerator {
    additional_functions: Vec<AdditionalFunction>,
    inclusions: String,
}

impl AdditionalCppGenerator {
    pub(crate) fn new(inclusions: String) -> Self {
        AdditionalCppGenerator {
            additional_functions: Vec::new(),
            inclusions,
        }
    }

    pub(crate) fn add_needs(
        &mut self,
        additions: Vec<AdditionalNeed>,
        type_database: &TypeDatabase,
    ) {
        for need in additions {
            match need {
                AdditionalNeed::MakeStringConstructor => self.generate_string_constructor(),
                AdditionalNeed::FunctionWrapper(by_value_wrapper) => {
                    self.generate_by_value_wrapper(*by_value_wrapper, type_database)
                }
            }
        }
    }

    pub(crate) fn generate(&self) -> Option<AdditionalCpp> {
        if self.additional_functions.is_empty() {
            None
        } else {
            let headers: HashSet<Header> = self
                .additional_functions
                .iter()
                .map(|x| x.headers.iter().cloned())
                .flatten()
                .collect();
            let headers = headers.iter().map(|x| x.include_stmt()).join("\n");
            let declarations = self.concat_additional_items(|x| &x.declaration);
            let declarations = format!("{}\n{}\n{}", headers, self.inclusions, declarations);
            let definitions = self.concat_additional_items(|x| &x.definition);
            let definitions = format!("#include \"autocxxgen.h\"\n{}", definitions);
            Some(AdditionalCpp {
                declarations,
                definitions,
            })
        }
    }

    fn concat_additional_items<F>(&self, field_access: F) -> String
    where
        F: FnMut(&AdditionalFunction) -> &str,
    {
        let mut s = self
            .additional_functions
            .iter()
            .map(field_access)
            .collect::<Vec<&str>>()
            .join("\n");
        s.push('\n');
        s
    }

    fn generate_string_constructor(&mut self) {
        let declaration = "std::unique_ptr<std::string> make_string(::rust::Str str)";
        let definition = format!(
            "{} {{ return std::make_unique<std::string>(std::string(str)); }}",
            declaration
        );
        let declaration = format!("{};", declaration);
        self.additional_functions.push(AdditionalFunction {
            declaration,
            definition,
            headers: vec![
                Header::system("memory"),
                Header::system("string"),
                Header::user("cxx.h"),
            ],
        })
    }

    fn generate_by_value_wrapper(
        &mut self,
        details: FunctionWrapper,
        type_database: &TypeDatabase,
    ) {
        // Even if the original function call is in a namespace,
        // we generate this wrapper in the global namespace.
        // We could easily do this the other way round, and when
        // cxx::bridge comes to support nested namespace mods then
        // we wil wish to do that to avoid name conflicts. However,
        // at the moment this is simpler because it avoids us having
        // to generate namespace blocks in the generated C++.
        let is_a_method = details.is_a_method;
        let name = details.wrapper_function_name;
        let get_arg_name = |counter: usize| -> String {
            if is_a_method && counter == 0 {
                // For method calls that we generate, the first
                // argument name needs to be such that we recognize
                // it as a method in the second invocation of
                // bridge_converter after it's flowed again through
                // bindgen.
                // TODO this may not be the case any longer. We
                // may be able to remove this.
                "autocxx_gen_this".to_string()
            } else {
                format!("arg{}", counter)
            }
        };
        let args = details
            .argument_conversion
            .iter()
            .enumerate()
            .map(|(counter, ty)| {
                format!(
                    "{} {}",
                    ty.unconverted_type(type_database),
                    get_arg_name(counter)
                )
            })
            .join(", ");
        let ret_type = details
            .return_conversion
            .as_ref()
            .map_or("void".to_string(), |x| x.converted_type(type_database));
        let declaration = format!("{} {}({})", ret_type, name, args);
        let mut arg_list = details
            .argument_conversion
            .iter()
            .enumerate()
            .map(|(counter, conv)| conv.conversion(&get_arg_name(counter), type_database));
        let receiver = if is_a_method { arg_list.next() } else { None };
        let arg_list = arg_list.join(", ");
        let mut underlying_function_call = match details.payload {
            FunctionWrapperPayload::Constructor => arg_list,
            FunctionWrapperPayload::FunctionCall(ns, id) => match receiver {
                Some(receiver) => format!("{}.{}({})", receiver, id.to_string(), arg_list),
                None => {
                    let underlying_function_call = ns
                        .into_iter()
                        .cloned()
                        .chain(std::iter::once(id.to_string()))
                        .join("::");
                    format!("{}({})", underlying_function_call, arg_list)
                }
            },
            FunctionWrapperPayload::StaticMethodCall(ns, ty_id, fn_id) => {
                let underlying_function_call = ns
                    .into_iter()
                    .cloned()
                    .chain([ty_id.to_string(), fn_id.to_string()].iter().cloned())
                    .join("::");
                format!("{}({})", underlying_function_call, arg_list)
            }
        };
        if let Some(ret) = details.return_conversion {
            underlying_function_call = format!(
                "return {}",
                ret.conversion(&underlying_function_call, type_database)
            );
        };
        let definition = format!("{} {{ {}; }}", declaration, underlying_function_call,);
        let declaration = format!("{};", declaration);
        self.additional_functions.push(AdditionalFunction {
            declaration,
            definition,
            headers: vec![Header::system("memory")],
        })
    }
}
