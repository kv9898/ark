//
// variable.rs
//
// Copyright (C) 2023 by Posit Software, PBC
//
//

use harp::call::RCall;
use harp::environment::Binding;
use harp::environment::BindingValue;
use harp::environment::Environment;
use harp::environment::EnvironmentFilter;
use harp::error::Error;
use harp::exec::r_try_catch;
use harp::exec::RFunction;
use harp::exec::RFunctionExt;
use harp::object::r_length;
use harp::object::r_list_get;
use harp::object::RObject;
use harp::r_symbol;
use harp::symbol::RSymbol;
use harp::utils::pairlist_size;
use harp::utils::r_altrep_class;
use harp::utils::r_assert_type;
use harp::utils::r_classes;
use harp::utils::r_inherits;
use harp::utils::r_is_altrep;
use harp::utils::r_is_data_frame;
use harp::utils::r_is_matrix;
use harp::utils::r_is_null;
use harp::utils::r_is_s4;
use harp::utils::r_is_simple_vector;
use harp::utils::r_is_unbound;
use harp::utils::r_typeof;
use harp::utils::r_vec_is_single_dimension_with_single_value;
use harp::utils::r_vec_shape;
use harp::utils::r_vec_type;
use harp::vector::formatted_vector::FormattedVector;
use harp::vector::names::Names;
use harp::vector::CharacterVector;
use harp::vector::IntegerVector;
use harp::vector::Vector;
use itertools::Itertools;
use libR_shim::*;
use serde::Deserialize;
use serde::Serialize;
use stdext::local;
use stdext::unwrap;

// Constants.
const MAX_DISPLAY_VALUE_ENTRIES: usize = 1_000;
const MAX_DISPLAY_VALUE_LENGTH: usize = 100;

/// Represents the supported kinds of variable values.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// A length-1 logical vector
    Boolean,

    /// A raw byte array
    Bytes,

    /// A collection of unnamed values; usually a vector
    Collection,

    /// Empty/missing values such as NULL, NA, or missing
    Empty,

    /// A function, method, closure, or other callable object
    Function,

    /// Named lists of values, such as lists and (hashed) environments
    Map,

    /// A number, such as an integer or floating-point value
    Number,

    /// A value of an unknown or unspecified type
    Other,

    /// A character string
    String,

    /// A table, dataframe, 2D matrix, or other two-dimensional data structure
    Table,

    /// Lazy: promise code
    Lazy,
}

/// Represents the serialized form of a variable.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Variable {
    /** The access key; not displayed to the user, but used to form path accessors */
    pub access_key: String,

    /** The variable's name, formatted for display */
    pub display_name: String,

    /** The variable's value, formatted for display */
    pub display_value: String,

    /** The variable's type, formatted for display */
    pub display_type: String,

    /** Extended type information */
    pub type_info: String,

    /** The variable's value kind (string, number, etc.) */
    pub kind: ValueKind,

    /** The number of elements in the variable's value, if applicable */
    pub length: usize,

    /** The size of the variable's value, in bytes */
    pub size: usize,

    /** True if the variable contains other variables */
    pub has_children: bool,

    /** True if the 'value' field was truncated to fit in the message */
    pub is_truncated: bool,

    /** True for things that can be View()ed */
    pub has_viewer: bool,
}

pub struct WorkspaceVariableDisplayValue {
    pub display_value: String,
    pub is_truncated: bool,
}

struct DimDataFrame {
    nrow: i32,
    ncol: i32,
}

fn dim_data_frame(data: SEXP) -> DimDataFrame {
    unsafe {
        let dim = RFunction::new("base", "dim.data.frame")
            .add(data)
            .call()
            .unwrap();

        DimDataFrame {
            nrow: INTEGER_ELT(*dim, 0),
            ncol: INTEGER_ELT(*dim, 1),
        }
    }
}

fn plural(text: &str, n: i32) -> String {
    if n == 1 {
        String::from(text)
    } else {
        format!("{}s", text)
    }
}

impl WorkspaceVariableDisplayValue {
    pub fn from(value: SEXP) -> Self {
        match r_typeof(value) {
            NILSXP => Self::new(String::from("NULL"), false),
            VECSXP if r_inherits(value, "data.frame") => Self::from_data_frame(value),
            VECSXP if !r_inherits(value, "POSIXlt") => Self::from_list(value),
            LISTSXP => Self::empty(),
            SYMSXP if value == unsafe { R_MissingArg } => {
                Self::new(String::from("<missing>"), false)
            },
            CLOSXP => Self::from_closure(value),
            ENVSXP => Self::from_env(value),
            _ if r_is_matrix(value) => Self::from_matrix(value),
            _ => Self::from_default(value),
        }
    }

    fn new(display_value: String, is_truncated: bool) -> Self {
        WorkspaceVariableDisplayValue {
            display_value,
            is_truncated,
        }
    }

    fn empty() -> Self {
        Self::new(String::from(""), false)
    }

    fn from_data_frame(value: SEXP) -> Self {
        let dim = dim_data_frame(value);
        let class = match r_classes(value) {
            None => String::from(""),
            Some(classes) => match classes.get_unchecked(0) {
                Some(class) => format!(" <{}>", class),
                None => String::from(""),
            },
        };

        let value = format!(
            "[{} {} x {} {}]{}",
            dim.nrow,
            plural("row", dim.nrow),
            dim.ncol,
            plural("column", dim.ncol),
            class
        );
        Self::new(value, false)
    }

    fn from_list(value: SEXP) -> Self {
        let n = r_length(value);
        let mut display_value = String::from("[");
        let mut is_truncated = false;
        let names = Names::new(value, |_i| String::from(""));

        for i in 0..n {
            if i > 0 {
                display_value.push_str(", ");
            }
            let display_i = Self::from(r_list_get(value, i));
            let name = names.get_unchecked(i);
            if !name.is_empty() {
                display_value.push_str(&name);
                display_value.push_str(" = ");
            }
            display_value.push_str(&display_i.display_value);

            if display_value.len() > MAX_DISPLAY_VALUE_LENGTH || display_i.is_truncated {
                is_truncated = true;
            }
        }

        display_value.push_str("]");
        Self::new(display_value, is_truncated)
    }

    fn from_closure(value: SEXP) -> Self {
        unsafe {
            let args = RFunction::from("args").add(value).call().unwrap();
            let formatted = RFunction::from("format").add(*args).call().unwrap();
            let formatted = CharacterVector::new_unchecked(formatted);
            let out = formatted
                .iter()
                .take(formatted.len() - 1)
                .map(|o| o.unwrap())
                .join("");
            Self::new(out, false)
        }
    }

    fn from_env(value: SEXP) -> Self {
        // Get the environment and its length (excluding hidden bindings)
        let environment = Environment::new(RObject::view(value));
        let environment_length = environment.length(EnvironmentFilter::ExcludeHiddenBindings);

        // If the environment is empty, return the empty display value. If the environment is
        // large, return the large display value (because it may be too expensive to sort the
        // bindings). Otherwise, return a detailed display value that shows some or all of the
        // bindings in the environment.
        if environment_length == 0 {
            return Self::new(String::from("Empty Environment [0 values]"), false);
        }

        if environment_length > MAX_DISPLAY_VALUE_ENTRIES {
            return Self::new(
                format!("Large Environment [{} values]", environment_length),
                true,
            );
        }

        // For environment we don't display values, only names. So we don't need to create a
        // Variable for each bindings as we used to, and which caused an infinite recursion since
        // environments may be self-referential (posit-dev/positron#1690).
        let names = environment.names();

        // Build the detailed display value
        let mut display_value = String::new();

        let env_name = environment.name();
        if let Some(env_name) = env_name {
            display_value.push_str(format!("{env_name}: ").as_str())
        }

        display_value.push_str("{");

        let mut is_truncated = false;
        for (i, name) in names
            .iter()
            .filter(|name| !name.starts_with("."))
            .sorted_by(|lhs, rhs| Ord::cmp(&lhs, &rhs))
            .enumerate()
        {
            // If this isn't the first entry, append a space separator.
            if i > 0 {
                display_value.push_str(", ");
            }

            // Append the variable display name.
            display_value.push_str(name);

            // When the display value becomes too long, mark it as truncated and stop
            // building it.
            if i == 10 || display_value.len() > MAX_DISPLAY_VALUE_LENGTH {
                // If there are remaining entries, set the is_truncated flag and append a
                // counter of how many more entries there are.
                let remaining_entries = environment_length - 1 - i;
                if remaining_entries > 0 {
                    is_truncated = true;
                    display_value.push_str(&format!(" [{} more]", remaining_entries));
                }

                // Stop building the display value.
                break;
            }
        }

        display_value.push_str("}");

        // Return the display value.
        Self::new(display_value, is_truncated)
    }

    // TODO: handle higher dimensional arrays, i.e. expand
    //       recursively from the higher dimension
    fn from_matrix(value: SEXP) -> Self {
        let formatted = unwrap!(FormattedVector::new(value), Err(err) => {
            return Self::from_error(err);
        });

        let mut first = true;
        let mut display_value = String::from("");
        let mut is_truncated = false;

        unsafe {
            let dim = IntegerVector::new_unchecked(Rf_getAttrib(value, R_DimSymbol));
            let n_col = dim.get_unchecked(1).unwrap() as isize;
            display_value.push_str("[");
            for i in 0..n_col {
                if first {
                    first = false;
                } else {
                    display_value.push_str(", ");
                }

                display_value.push_str("[");
                let display_column = formatted.column_iter(i).join(" ");
                if display_column.len() > MAX_DISPLAY_VALUE_LENGTH {
                    is_truncated = true;
                    // TODO: maybe this should only push_str() a slice
                    //       of the first n (MAX_WIDTH?) characters in that case ?
                }
                display_value.push_str(display_column.as_str());
                display_value.push_str("]");

                if display_value.len() > MAX_DISPLAY_VALUE_LENGTH {
                    is_truncated = true;
                }
                if is_truncated {
                    break;
                }
            }
            display_value.push_str("]");
        }
        Self::new(display_value, is_truncated)
    }

    fn from_default(value: SEXP) -> Self {
        let formatted = unwrap!(FormattedVector::new(value), Err(err) => {
            return Self::from_error(err);
        });

        let mut first = true;
        let mut display_value = String::from("");
        let mut is_truncated = false;

        for x in formatted.iter() {
            if first {
                first = false;
            } else {
                display_value.push_str(" ");
            }
            display_value.push_str(&x);
            if display_value.len() > MAX_DISPLAY_VALUE_LENGTH {
                is_truncated = true;
                break;
            }
        }

        Self::new(display_value, is_truncated)
    }

    fn from_error(err: Error) -> Self {
        log::warn!("Error while formatting variable: {err:?}");
        Self::new(String::from("??"), true)
    }
}

pub struct WorkspaceVariableDisplayType {
    pub display_type: String,
    pub type_info: String,
}

impl WorkspaceVariableDisplayType {
    pub fn from(value: SEXP) -> Self {
        if r_is_null(value) {
            return Self::simple(String::from("NULL"));
        }

        if r_is_s4(value) {
            return Self::from_class(value, String::from("S4"));
        }

        if r_is_simple_vector(value) {
            let display_type: String;
            if r_vec_is_single_dimension_with_single_value(value) {
                display_type = r_vec_type(value);
            } else {
                display_type = format!("{} [{}]", r_vec_type(value), r_vec_shape(value));
            }

            let mut type_info = display_type.clone();
            if r_is_altrep(value) {
                type_info.push_str(r_altrep_class(value).as_str())
            }

            return Self::new(display_type, type_info);
        }

        let rtype = r_typeof(value);
        match rtype {
            EXPRSXP => {
                let default = format!("expression [{}]", unsafe { XLENGTH(value) });
                Self::from_class(value, default)
            },
            LANGSXP => Self::from_class(value, String::from("language")),
            CLOSXP => Self::from_class(value, String::from("function")),
            ENVSXP => Self::from_class(value, String::from("environment")),
            SYMSXP => {
                if r_is_null(value) {
                    Self::simple(String::from("missing"))
                } else {
                    Self::simple(String::from("symbol"))
                }
            },

            LISTSXP => match pairlist_size(value) {
                Ok(n) => Self::simple(format!("pairlist [{}]", n)),
                Err(_) => Self::simple(String::from("pairlist [?]")),
            },

            VECSXP => unsafe {
                if r_is_data_frame(value) {
                    let classes = r_classes(value).unwrap();
                    let dfclass = classes.get_unchecked(0).unwrap();

                    let dim = RFunction::new("base", "dim.data.frame")
                        .add(value)
                        .call()
                        .unwrap();
                    let shape = FormattedVector::new(*dim).unwrap().iter().join(", ");
                    let display_type = format!("{} [{}]", dfclass, shape);
                    Self::simple(display_type)
                } else {
                    let default = format!("list [{}]", XLENGTH(value));
                    Self::from_class(value, default)
                }
            },
            _ => Self::from_class(value, String::from("???")),
        }
    }

    fn simple(display_type: String) -> Self {
        Self {
            display_type,
            type_info: String::from(""),
        }
    }

    fn from_class(value: SEXP, default: String) -> Self {
        match r_classes(value) {
            None => Self::simple(default),
            Some(classes) => Self::new(
                classes.get_unchecked(0).unwrap(),
                classes.iter().map(|s| s.unwrap()).join("/"),
            ),
        }
    }

    fn new(display_type: String, type_info: String) -> Self {
        Self {
            display_type,
            type_info,
        }
    }
}

fn has_children(value: SEXP) -> bool {
    if RObject::view(value).is_s4() {
        unsafe {
            let names = RFunction::new("methods", ".slotNames")
                .add(value)
                .call()
                .unwrap();
            let names = CharacterVector::new_unchecked(names);
            names.len() > 0
        }
    } else {
        match r_typeof(value) {
            VECSXP | EXPRSXP => unsafe { XLENGTH(value) != 0 },
            LISTSXP => true,
            ENVSXP => !Environment::new(RObject::view(value))
                .is_empty(EnvironmentFilter::ExcludeHiddenBindings),
            LGLSXP | RAWSXP | STRSXP | INTSXP | REALSXP | CPLXSXP => unsafe { XLENGTH(value) > 1 },
            _ => false,
        }
    }
}

enum EnvironmentVariableNode {
    Concrete { object: RObject },
    Artificial { object: RObject, name: String },
    Matrixcolumn { object: RObject, index: isize },
    VectorElement { object: RObject, index: isize },
}

impl Variable {
    /**
     * Create a new Variable from a Binding
     */
    pub fn new(binding: &Binding) -> Self {
        let display_name = binding.name.to_string();

        match &binding.value {
            BindingValue::Active { .. } => Self::from_active_binding(display_name),
            BindingValue::Promise { promise } => Self::from_promise(display_name, promise.sexp),
            BindingValue::Altrep { object, .. } | BindingValue::Standard { object, .. } => {
                Self::from(display_name.clone(), display_name, object.sexp)
            },
        }
    }

    /**
     * Create a new Variable from an R object
     */
    fn from(access_key: String, display_name: String, x: SEXP) -> Self {
        let WorkspaceVariableDisplayValue {
            display_value,
            is_truncated,
        } = WorkspaceVariableDisplayValue::from(x);
        let WorkspaceVariableDisplayType {
            display_type,
            type_info,
        } = WorkspaceVariableDisplayType::from(x);

        let kind = Self::variable_kind(x);

        Self {
            access_key,
            display_name,
            display_value,
            display_type,
            type_info,
            kind,
            length: Self::variable_length(x),
            size: RObject::view(x).size(),
            has_children: has_children(x),
            is_truncated,
            has_viewer: r_is_data_frame(x) || r_is_matrix(x),
        }
    }

    fn from_promise(display_name: String, promise: SEXP) -> Self {
        let display_value = local! {
            unsafe {
                let code = PRCODE(promise);
                match r_typeof(code) {
                    SYMSXP => {
                        Ok(RSymbol::new_unchecked(code).to_string())
                    },
                    LANGSXP => {
                        let code = RCall::new(code)?;
                        let fun = RSymbol::new(CAR(*code))?;
                        if fun == "lazyLoadDBfetch" {
                            return Ok(String::from("(unevaluated)"))
                        }

                        RFunction::from(".ps.environment.describeCall")
                            .add(code)
                            .call()?
                            .try_into()
                    },
                    _ => Err(Error::UnexpectedType(r_typeof(code), vec!(SYMSXP, LANGSXP)))
                }
            }
        };

        Self {
            access_key: display_name.clone(),
            display_name,
            display_value: display_value.unwrap_or(String::from("(unevaluated)")),
            display_type: String::from("promise"),
            type_info: String::from("promise"),
            kind: ValueKind::Lazy,
            length: 0,
            size: 0,
            has_children: false,
            is_truncated: false,
            has_viewer: false,
        }
    }

    fn from_active_binding(display_name: String) -> Self {
        Self {
            access_key: display_name.clone(),
            display_name,
            display_value: String::from(""),
            display_type: String::from("active binding"),
            type_info: String::from("active binding"),
            kind: ValueKind::Other,
            length: 0,
            size: 0,
            has_children: false,
            is_truncated: false,
            has_viewer: false,
        }
    }

    fn variable_length(x: SEXP) -> usize {
        let rtype = r_typeof(x);
        match rtype {
            LGLSXP | RAWSXP | INTSXP | REALSXP | CPLXSXP | STRSXP => unsafe { XLENGTH(x) as usize },
            VECSXP => unsafe {
                if r_inherits(x, "POSIXlt") {
                    XLENGTH(VECTOR_ELT(x, 0)) as usize
                } else if r_is_data_frame(x) {
                    let dim = RFunction::new("base", "dim.data.frame")
                        .add(x)
                        .call()
                        .unwrap();

                    INTEGER_ELT(*dim, 0) as usize
                } else {
                    XLENGTH(x) as usize
                }
            },
            LISTSXP => match pairlist_size(x) {
                Ok(n) => n as usize,
                Err(_) => 0,
            },
            _ => 0,
        }
    }

    fn variable_kind(x: SEXP) -> ValueKind {
        if x == unsafe { R_NilValue } {
            return ValueKind::Empty;
        }

        let obj = RObject::view(x);

        if obj.is_s4() {
            return ValueKind::Map;
        }

        if r_inherits(x, "factor") {
            return ValueKind::Other;
        }

        if r_is_data_frame(x) {
            return ValueKind::Table;
        }

        // TODO: generic S3 object, not sure what it should be

        match r_typeof(x) {
            CLOSXP => ValueKind::Function,

            ENVSXP => {
                // this includes R6 objects
                ValueKind::Map
            },

            VECSXP => unsafe {
                let dim = Rf_getAttrib(x, R_DimSymbol);
                if dim != R_NilValue && XLENGTH(dim) == 2 {
                    ValueKind::Table
                } else {
                    ValueKind::Map
                }
            },

            LGLSXP => unsafe {
                let dim = Rf_getAttrib(x, R_DimSymbol);
                if dim != R_NilValue && XLENGTH(dim) == 2 {
                    ValueKind::Table
                } else if XLENGTH(x) == 1 {
                    if LOGICAL_ELT(x, 0) == R_NaInt {
                        ValueKind::Empty
                    } else {
                        ValueKind::Boolean
                    }
                } else {
                    ValueKind::Collection
                }
            },

            INTSXP => unsafe {
                let dim = Rf_getAttrib(x, R_DimSymbol);
                if dim != R_NilValue && XLENGTH(dim) == 2 {
                    ValueKind::Table
                } else if XLENGTH(x) == 1 {
                    if INTEGER_ELT(x, 0) == R_NaInt {
                        ValueKind::Empty
                    } else {
                        ValueKind::Number
                    }
                } else {
                    ValueKind::Collection
                }
            },

            REALSXP => unsafe {
                let dim = Rf_getAttrib(x, R_DimSymbol);
                if dim != R_NilValue && XLENGTH(dim) == 2 {
                    ValueKind::Table
                } else if XLENGTH(x) == 1 {
                    if R_IsNA(REAL_ELT(x, 0)) == 1 {
                        ValueKind::Empty
                    } else {
                        ValueKind::Number
                    }
                } else {
                    ValueKind::Collection
                }
            },

            CPLXSXP => unsafe {
                let dim = Rf_getAttrib(x, R_DimSymbol);
                if dim != R_NilValue && XLENGTH(dim) == 2 {
                    ValueKind::Table
                } else if XLENGTH(x) == 1 {
                    let value = COMPLEX_ELT(x, 0);
                    if R_IsNA(value.r) == 1 || R_IsNA(value.i) == 1 {
                        ValueKind::Empty
                    } else {
                        ValueKind::Number
                    }
                } else {
                    ValueKind::Collection
                }
            },

            STRSXP => unsafe {
                let dim = Rf_getAttrib(x, R_DimSymbol);
                if dim != R_NilValue && XLENGTH(dim) == 2 {
                    ValueKind::Table
                } else if XLENGTH(x) == 1 {
                    if STRING_ELT(x, 0) == R_NaString {
                        ValueKind::Empty
                    } else {
                        ValueKind::String
                    }
                } else {
                    ValueKind::Collection
                }
            },

            RAWSXP => ValueKind::Bytes,
            _ => ValueKind::Other,
        }
    }

    pub fn inspect(env: RObject, path: &Vec<String>) -> Result<Vec<Self>, harp::error::Error> {
        let node = unsafe { Self::resolve_object_from_path(env, &path)? };

        match node {
            EnvironmentVariableNode::Artificial { object, name } => match name.as_str() {
                "<private>" => {
                    let env = Environment::new(object);
                    let enclos = Environment::new(RObject::view(env.find(".__enclos_env__")));
                    let private = RObject::view(enclos.find("private"));

                    Self::inspect_environment(private)
                },

                "<methods>" => Self::inspect_r6_methods(object),

                _ => Err(harp::error::Error::InspectError { path: path.clone() }),
            },

            EnvironmentVariableNode::Concrete { object } => {
                if object.is_s4() {
                    Self::inspect_s4(*object)
                } else {
                    match r_typeof(*object) {
                        VECSXP | EXPRSXP => Self::inspect_list(*object),
                        LISTSXP => Self::inspect_pairlist(*object),
                        ENVSXP => {
                            if r_inherits(*object, "R6") {
                                Self::inspect_r6(object)
                            } else {
                                Self::inspect_environment(object)
                            }
                        },
                        LGLSXP | RAWSXP | STRSXP | INTSXP | REALSXP | CPLXSXP => {
                            if r_is_matrix(*object) {
                                Self::inspect_matrix(*object)
                            } else {
                                Self::inspect_vector(*object)
                            }
                        },
                        _ => Ok(vec![]),
                    }
                }
            },

            EnvironmentVariableNode::Matrixcolumn { object, index } => {
                Self::inspect_matrix_column(*object, index)
            },
            EnvironmentVariableNode::VectorElement { .. } => Ok(vec![]),
        }
    }

    pub fn clip(
        env: RObject,
        path: &Vec<String>,
        _format: &String,
    ) -> Result<String, harp::error::Error> {
        let node = unsafe { Self::resolve_object_from_path(env, &path)? };

        match node {
            EnvironmentVariableNode::Concrete { object } => {
                if r_is_data_frame(*object) {
                    let formatted = RFunction::from(".ps.environment.clipboardFormatDataFrame")
                        .add(object)
                        .call()?;

                    Ok(FormattedVector::new(*formatted)?.iter().join("\n"))
                } else if r_typeof(*object) == CLOSXP {
                    let deparsed: Vec<String> =
                        RFunction::from("deparse").add(*object).call()?.try_into()?;

                    Ok(deparsed.join("\n"))
                } else {
                    Ok(FormattedVector::new(*object)?.iter().join(" "))
                }
            },
            EnvironmentVariableNode::Artificial { .. } => Ok(String::from("")),
            EnvironmentVariableNode::VectorElement { object, index } => {
                let formatted = FormattedVector::new(*object)?;
                Ok(formatted.get_unchecked(index))
            },
            EnvironmentVariableNode::Matrixcolumn { object, index } => unsafe {
                let dim = IntegerVector::new(Rf_getAttrib(*object, R_DimSymbol))?;
                let n_row = dim.get_unchecked(0).unwrap() as usize;

                let clipped = FormattedVector::new(*object)?
                    .iter()
                    .skip(index as usize * n_row)
                    .take(n_row)
                    .join(" ");
                Ok(clipped)
            },
        }
    }

    pub fn resolve_data_object(
        env: RObject,
        path: &Vec<String>,
    ) -> Result<RObject, harp::error::Error> {
        let resolved = unsafe { Self::resolve_object_from_path(env, path)? };

        match resolved {
            EnvironmentVariableNode::Concrete { object } => Ok(object),

            _ => Err(harp::error::Error::InspectError { path: path.clone() }),
        }
    }

    unsafe fn resolve_object_from_path(
        object: RObject,
        path: &Vec<String>,
    ) -> Result<EnvironmentVariableNode, harp::error::Error> {
        let mut node = EnvironmentVariableNode::Concrete { object };

        for path_element in path {
            node = match node {
                EnvironmentVariableNode::Concrete { object } => {
                    if object.is_s4() {
                        let name = r_symbol!(path_element);
                        let child = r_try_catch(|| R_do_slot(*object, name))?;
                        EnvironmentVariableNode::Concrete { object: child }
                    } else {
                        let rtype = r_typeof(*object);
                        match rtype {
                            ENVSXP => {
                                if r_inherits(*object, "R6") && path_element.starts_with("<") {
                                    EnvironmentVariableNode::Artificial {
                                        object,
                                        name: path_element.clone(),
                                    }
                                } else {
                                    let symbol = r_symbol!(path_element);
                                    let mut x = Rf_findVarInFrame(*object, symbol);

                                    if r_typeof(x) == PROMSXP {
                                        // if we are here, it means the promise is either evaluated
                                        // already, i.e. PRVALUE() is bound or it is a promise to
                                        // something that is not a call or a symbol because it would
                                        // have been handled in Binding::new()

                                        // Actual promises, i.e. unevaluated promises can't be
                                        // expanded in the variables pane so we would not get here.

                                        let value = PRVALUE(x);
                                        if r_is_unbound(value) {
                                            x = PRCODE(x);
                                        } else {
                                            x = value;
                                        }
                                    }

                                    EnvironmentVariableNode::Concrete {
                                        object: RObject::view(x),
                                    }
                                }
                            },

                            VECSXP | EXPRSXP => {
                                let index = path_element.parse::<isize>().unwrap();
                                EnvironmentVariableNode::Concrete {
                                    object: RObject::view(VECTOR_ELT(*object, index)),
                                }
                            },

                            LISTSXP => {
                                let mut pairlist = *object;
                                let index = path_element.parse::<isize>().unwrap();
                                for _i in 0..index {
                                    pairlist = CDR(pairlist);
                                }
                                EnvironmentVariableNode::Concrete {
                                    object: RObject::view(CAR(pairlist)),
                                }
                            },

                            LGLSXP | RAWSXP | STRSXP | INTSXP | REALSXP | CPLXSXP => {
                                if r_is_matrix(*object) {
                                    EnvironmentVariableNode::Matrixcolumn {
                                        object,
                                        index: path_element.parse::<isize>().unwrap(),
                                    }
                                } else {
                                    EnvironmentVariableNode::VectorElement {
                                        object,
                                        index: path_element.parse::<isize>().unwrap(),
                                    }
                                }
                            },

                            _ => {
                                return Err(harp::error::Error::InspectError { path: path.clone() })
                            },
                        }
                    }
                },

                EnvironmentVariableNode::Artificial { object, name } => {
                    match name.as_str() {
                        "<private>" => {
                            let env = Environment::new(object);
                            let enclos =
                                Environment::new(RObject::view(env.find(".__enclos_env__")));
                            let private = Environment::new(RObject::view(enclos.find("private")));

                            // TODO: it seems unlikely that private would host active bindings
                            //       so find() is fine, we can assume this is concrete
                            EnvironmentVariableNode::Concrete {
                                object: RObject::view(private.find(path_element)),
                            }
                        },

                        _ => return Err(harp::error::Error::InspectError { path: path.clone() }),
                    }
                },

                EnvironmentVariableNode::VectorElement { .. } => {
                    return Err(harp::error::Error::InspectError { path: path.clone() });
                },

                EnvironmentVariableNode::Matrixcolumn { object, index } => unsafe {
                    let dim = IntegerVector::new(Rf_getAttrib(*object, R_DimSymbol))?;
                    let n_row = dim.get_unchecked(0).unwrap() as isize;

                    // TODO: use ? here, but this does not return a crate::error::Error, so
                    //       maybe use anyhow here instead ?
                    let row_index = path_element.parse::<isize>().unwrap();

                    EnvironmentVariableNode::VectorElement {
                        object,
                        index: n_row * index + row_index,
                    }
                },
            }
        }

        Ok(node)
    }

    fn inspect_list(value: SEXP) -> Result<Vec<Self>, harp::error::Error> {
        let mut out: Vec<Self> = vec![];
        let n = unsafe { XLENGTH(value) };

        let names = Names::new(value, |i| format!("[[{}]]", i + 1));

        for i in 0..n {
            let obj = unsafe { VECTOR_ELT(value, i) };
            out.push(Self::from(i.to_string(), names.get_unchecked(i), obj));
        }

        Ok(out)
    }

    fn inspect_matrix(matrix: SEXP) -> harp::error::Result<Vec<Self>> {
        unsafe {
            let matrix = RObject::new(matrix);
            let dim = IntegerVector::new(Rf_getAttrib(*matrix, R_DimSymbol))?;

            let n_col = dim.get_unchecked(1).unwrap();

            let mut out: Vec<Self> = vec![];
            let formatted = FormattedVector::new(*matrix)?;

            for i in 0..n_col {
                let display_value = format!("[{}]", formatted.column_iter(i as isize).join(", "));
                out.push(Self {
                    access_key: format!("{}", i),
                    display_name: format!("[, {}]", i + 1),
                    display_value,
                    display_type: String::from(""),
                    type_info: String::from(""),
                    kind: ValueKind::Collection,
                    length: 1,
                    size: 0,
                    has_children: true,
                    is_truncated: false,
                    has_viewer: false,
                });
            }

            Ok(out)
        }
    }

    fn inspect_matrix_column(matrix: SEXP, index: isize) -> harp::error::Result<Vec<Self>> {
        unsafe {
            let matrix = RObject::new(matrix);
            let dim = IntegerVector::new(Rf_getAttrib(*matrix, R_DimSymbol))?;

            let n_row = dim.get_unchecked(0).unwrap();

            let mut out: Vec<Self> = vec![];
            let formatted = FormattedVector::new(*matrix)?;
            let mut iter = formatted.column_iter(index);
            let r_type = r_typeof(*matrix);
            let kind = if r_type == STRSXP {
                ValueKind::String
            } else if r_type == RAWSXP {
                ValueKind::Bytes
            } else if r_type == LGLSXP {
                ValueKind::Boolean
            } else {
                ValueKind::Number
            };

            for i in 0..n_row {
                out.push(Self {
                    access_key: format!("{}", i),
                    display_name: format!("[{}, {}]", i + 1, index + 1),
                    display_value: iter.next().unwrap(),
                    display_type: String::from(""),
                    type_info: String::from(""),
                    kind,
                    length: 1,
                    size: 0,
                    has_children: false,
                    is_truncated: false,
                    has_viewer: false,
                });
            }

            Ok(out)
        }
    }

    fn inspect_vector(vector: SEXP) -> harp::error::Result<Vec<Self>> {
        unsafe {
            let vector = RObject::new(vector);
            let n = XLENGTH(*vector);

            let mut out: Vec<Self> = vec![];
            let r_type = r_typeof(*vector);
            let formatted = FormattedVector::new(*vector)?;
            let names = Names::new(*vector, |i| format!("[{}]", i + 1));
            let kind = if r_type == STRSXP {
                ValueKind::String
            } else if r_type == RAWSXP {
                ValueKind::Bytes
            } else if r_type == LGLSXP {
                ValueKind::Boolean
            } else {
                ValueKind::Number
            };

            for i in 0..n {
                out.push(Self {
                    access_key: format!("{}", i),
                    display_name: names.get_unchecked(i),
                    display_value: formatted.get_unchecked(i),
                    display_type: String::from(""),
                    type_info: String::from(""),
                    kind,
                    length: 1,
                    size: 0,
                    has_children: false,
                    is_truncated: false,
                    has_viewer: false,
                });
            }

            Ok(out)
        }
    }

    fn inspect_pairlist(value: SEXP) -> Result<Vec<Self>, harp::error::Error> {
        let mut out: Vec<Self> = vec![];

        let mut pairlist = value;
        unsafe {
            let mut i = 0;
            while pairlist != R_NilValue {
                r_assert_type(pairlist, &[LISTSXP])?;

                let tag = TAG(pairlist);
                let display_name = if r_is_null(tag) {
                    format!("[[{}]]", i + 1)
                } else {
                    String::from(RSymbol::new_unchecked(tag))
                };

                out.push(Self::from(i.to_string(), display_name, CAR(pairlist)));

                pairlist = CDR(pairlist);
                i = i + 1;
            }
        }

        Ok(out)
    }

    fn inspect_r6(value: RObject) -> Result<Vec<Self>, harp::error::Error> {
        let mut has_private = false;
        let mut has_methods = false;

        let env = Environment::new(value);
        let mut childs: Vec<Self> = env
            .iter()
            .filter(|b: &Binding| {
                if b.name == ".__enclos_env__" {
                    if let BindingValue::Standard { object, .. } = &b.value {
                        has_private =
                            Environment::new(RObject::view(object.sexp)).exists("private");
                    }

                    false
                } else if b.is_hidden() {
                    false
                } else {
                    match &b.value {
                        BindingValue::Standard { object, .. } |
                        BindingValue::Altrep { object, .. } => {
                            if r_typeof(object.sexp) == CLOSXP {
                                has_methods = true;
                                false
                            } else {
                                true
                            }
                        },

                        // active bindings and promises
                        _ => true,
                    }
                }
            })
            .map(|b| Self::new(&b))
            .collect();

        childs.sort_by(|a, b| a.display_name.cmp(&b.display_name));

        if has_private {
            childs.push(Self {
                access_key: String::from("<private>"),
                display_name: String::from("private"),
                display_value: String::from("Private fields and methods"),
                display_type: String::from(""),
                type_info: String::from(""),
                kind: ValueKind::Other,
                length: 0,
                size: 0,
                has_children: true,
                is_truncated: false,
                has_viewer: false,
            });
        }

        if has_methods {
            childs.push(Self {
                access_key: String::from("<methods>"),
                display_name: String::from("methods"),
                display_value: String::from("Methods"),
                display_type: String::from(""),
                type_info: String::from(""),
                kind: ValueKind::Other,
                length: 0,
                size: 0,
                has_children: true,
                is_truncated: false,
                has_viewer: false,
            });
        }

        Ok(childs)
    }

    fn inspect_environment(value: RObject) -> Result<Vec<Self>, harp::error::Error> {
        let mut out: Vec<Self> = Environment::new(value)
            .iter()
            .filter(|b: &Binding| !b.is_hidden())
            .map(|b| Self::new(&b))
            .collect();

        out.sort_by(|a, b| a.display_name.cmp(&b.display_name));

        Ok(out)
    }

    fn inspect_s4(value: SEXP) -> Result<Vec<Self>, harp::error::Error> {
        let mut out: Vec<Self> = vec![];

        unsafe {
            let slot_names = RFunction::new("methods", ".slotNames").add(value).call()?;

            let slot_names = CharacterVector::new_unchecked(*slot_names);
            let mut iter = slot_names.iter();
            while let Some(Some(display_name)) = iter.next() {
                let slot_symbol = r_symbol!(display_name);
                let slot = r_try_catch(|| R_do_slot(value, slot_symbol))?;
                let access_key = display_name.clone();
                out.push(Variable::from(access_key, display_name, *slot));
            }
        }

        Ok(out)
    }

    fn inspect_r6_methods(value: RObject) -> Result<Vec<Self>, harp::error::Error> {
        let mut out: Vec<Self> = Environment::new(value)
            .iter()
            .filter(|b: &Binding| match &b.value {
                BindingValue::Standard { object, .. } => r_typeof(object.sexp) == CLOSXP,

                _ => false,
            })
            .map(|b| Self::new(&b))
            .collect();

        out.sort_by(|a, b| a.display_name.cmp(&b.display_name));

        Ok(out)
    }
}