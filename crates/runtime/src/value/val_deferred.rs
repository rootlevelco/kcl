//! Deferred values: attributes whose value is known only after evaluation,
//! for example the output of a resource that an orchestrator reconciles
//! later. A deferred value passes type checks as its declared type, builds
//! templates and conditions when used in strings and comparisons, and is
//! written to the output as a marker the orchestrator understands.
//!
//! A schema attribute becomes deferred with the `@computed` decorator. It
//! then refers to that attribute of that schema instance; instances are told
//! apart by an id (`InstanceId`) that survives copies.

use std::collections::HashMap;

use crate::*;

/// Output marker keys.
pub const DEFERRED_REF_KEY: &str = "_ref";
pub const DEFERRED_TEMPLATE_KEY: &str = "_template";
pub const DEFERRED_COND_KEY: &str = "_cond";
pub const DEFERRED_UNNAMED_KEY: &str = "_unnamed";
pub const COMPUTED_DECORATOR: &str = "computed";

/// The id of a schema instance. Ids don't take part in equality, so two
/// instances with the same attributes still compare equal.
#[derive(Clone, Copy, Default, Debug, Hash)]
pub struct InstanceId(pub u64);

impl PartialEq for InstanceId {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for InstanceId {}

#[derive(Clone, Debug, PartialEq)]
pub struct DeferredValue {
    pub expr: DeferredExpr,
    /// The declared type, e.g. `str`. `any` below a reference.
    pub ty: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeferredExpr {
    /// An attribute of a schema instance, and keys below it.
    Ref {
        oid: u64,
        schema: String,
        attr: String,
        path: Vec<String>,
    },
    /// A string built from literal parts and deferred values.
    Template(Vec<ValueRef>),
    /// A condition: `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `contains`, `and`,
    /// `or` or `not` over its arguments.
    Op { op: String, args: Vec<ValueRef> },
    /// A value kept opaque during evaluation and written as `{marker: value}`,
    /// e.g. a secret.
    Wrap { marker: String, value: ValueRef },
}

/// How instances are named, and what is known about deferred values.
#[derive(Clone, Debug, Default)]
pub struct DeferredOptions {
    /// Instances with this attribute are named when a top-level variable of
    /// the main package is assigned. Empty turns naming off.
    pub kind_attr: String,
    /// The attribute that holds an instance's name.
    pub name_attr: String,
    /// Known values as JSON, keyed by `<kind>/<name>.<attr>[.<key>...]`.
    pub known: String,
}

#[derive(Clone, Debug, Default)]
pub struct DeferredState {
    pub opts: DeferredOptions,
    next_oid: u64,
    /// `(kind, name)` of every named instance.
    pub names: HashMap<u64, (String, String)>,
    known: Option<HashMap<String, serde_json::Value>>,
    /// Whether any deferred value was created, so planning can skip the
    /// marker pass otherwise.
    pub used: bool,
}

impl DeferredState {
    pub fn new_oid(&mut self) -> u64 {
        self.next_oid += 1;
        self.next_oid
    }

    fn known_map(&mut self) -> &HashMap<String, serde_json::Value> {
        if self.known.is_none() {
            let map = if self.opts.known.is_empty() {
                HashMap::new()
            } else {
                serde_json::from_str(&self.opts.known)
                    .unwrap_or_else(|e| panic!("invalid known values for deferred attributes: {e}"))
            };
            self.known = Some(map);
        }
        self.known.as_ref().unwrap()
    }

    /// The key a reference is known under, once its instance has a name.
    pub fn ref_key(&self, v: &DeferredValue) -> Option<String> {
        match &v.expr {
            DeferredExpr::Ref {
                oid, attr, path, ..
            } => {
                let (kind, name) = self.names.get(oid)?;
                let mut key = format!("{kind}/{name}.{attr}");
                for p in path {
                    key.push('.');
                    key.push_str(p);
                }
                Some(key)
            }
            _ => None,
        }
    }
}

impl ValueRef {
    pub fn deferred(expr: DeferredExpr, ty: &str) -> Self {
        Self::from(Value::deferred_value(Box::new(DeferredValue {
            expr,
            ty: ty.to_string(),
        })))
    }

    #[inline]
    pub fn is_deferred(&self) -> bool {
        matches!(&*self.rc.borrow(), Value::deferred_value(_))
    }

    pub fn as_deferred(&self) -> Option<DeferredValue> {
        match &*self.rc.borrow() {
            Value::deferred_value(v) => Some((**v).clone()),
            _ => None,
        }
    }

    /// `self` with `key` below it: `v.key`, `v["key"]`, `v[0]`.
    pub fn deferred_load(&self, key: &str) -> Self {
        let d = self.as_deferred().expect("deferred value");
        match d.expr {
            DeferredExpr::Ref {
                oid,
                schema,
                attr,
                mut path,
            } => {
                path.push(key.to_string());
                Self::deferred(
                    DeferredExpr::Ref {
                        oid,
                        schema,
                        attr,
                        path,
                    },
                    "any",
                )
            }
            _ => panic!(
                "{} is computed at runtime and has no attribute '{key}'",
                self.deferred_repr()
            ),
        }
    }

    /// `lhs + rhs` when either side is deferred: string concatenation.
    pub fn deferred_concat(lhs: &Self, rhs: &Self) -> Self {
        Self::deferred_join(lhs, rhs, false)
    }

    /// A deferred value interpolated into a string, `"...${v}..."`: any type
    /// is written as a string.
    pub fn deferred_interpolate(lhs: &Self, rhs: &Self) -> Self {
        Self::deferred_join(lhs, rhs, true)
    }

    fn deferred_join(lhs: &Self, rhs: &Self, interpolated: bool) -> Self {
        let mut parts = Vec::new();
        for side in [lhs, rhs] {
            if let Some(d) = side.as_deferred() {
                match d.expr {
                    DeferredExpr::Template(ps) => parts.extend(ps),
                    DeferredExpr::Ref { .. } | DeferredExpr::Wrap { .. }
                        if interpolated || matches!(d.ty.as_str(), "str" | "any") =>
                    {
                        parts.push(side.clone())
                    }
                    _ => panic!(
                        "{} is computed at runtime; '+' only joins strings. Use it in `when` or a string template",
                        side.deferred_repr()
                    ),
                }
            } else if side.is_str() {
                if !side.as_str().is_empty() {
                    parts.push(side.clone());
                }
            } else {
                panic!(
                    "'+' of a value computed at runtime and '{}': computed values only join strings",
                    side.type_str()
                );
            }
        }
        Self::deferred(DeferredExpr::Template(parts), "str")
    }

    /// A condition over `args`.
    pub fn deferred_op(op: &str, args: Vec<Self>) -> Self {
        Self::deferred(
            DeferredExpr::Op {
                op: op.to_string(),
                args,
            },
            "bool",
        )
    }

    /// A short description for error messages.
    pub fn deferred_repr(&self) -> String {
        match self.as_deferred().map(|d| d.expr) {
            Some(DeferredExpr::Ref {
                schema, attr, path, ..
            }) => {
                let mut s = format!("{schema}.{attr}");
                for p in path {
                    s.push('.');
                    s.push_str(&p);
                }
                s
            }
            Some(DeferredExpr::Template(_)) => "a string built from computed values".to_string(),
            Some(DeferredExpr::Op { op, .. }) => format!("a '{op}' condition on computed values"),
            Some(DeferredExpr::Wrap { marker, .. }) => format!("a value marked {marker}"),
            None => self.type_str(),
        }
    }

    /// Replaces deferred values below `self` with output markers, in place.
    pub fn lower_deferred(&self, state: &DeferredState) -> Self {
        if let Some(d) = self.as_deferred() {
            return deferred_marker(&d, state);
        }
        match &mut *self.rc.borrow_mut() {
            Value::list_value(l) => {
                for v in l.values.iter_mut() {
                    *v = v.lower_deferred(state);
                }
            }
            Value::dict_value(d) => lower_dict(d, state),
            Value::schema_value(s) => lower_dict(&mut s.config, state),
            _ => {}
        }
        self.clone()
    }
}

fn lower_dict(d: &mut DictValue, state: &DeferredState) {
    for v in d.values.values_mut() {
        *v = v.lower_deferred(state);
    }
}

fn deferred_marker(d: &DeferredValue, state: &DeferredState) -> ValueRef {
    match &d.expr {
        DeferredExpr::Ref {
            schema, attr, path, ..
        } => match state.ref_key(d) {
            Some(key) => ValueRef::dict(Some(&[(DEFERRED_REF_KEY, &ValueRef::str(&key))])),
            None => {
                let mut what = format!("{schema}.{attr}");
                for p in path {
                    what.push('.');
                    what.push_str(p);
                }
                ValueRef::dict(Some(&[(DEFERRED_UNNAMED_KEY, &ValueRef::str(&what))]))
            }
        },
        DeferredExpr::Template(parts) => {
            let parts: Vec<ValueRef> = parts.iter().map(|p| p.lower_deferred(state)).collect();
            let refs: Vec<&ValueRef> = parts.iter().collect();
            ValueRef::dict(Some(&[(
                DEFERRED_TEMPLATE_KEY,
                &ValueRef::list(Some(&refs)),
            )]))
        }
        DeferredExpr::Wrap { marker, value } => {
            ValueRef::dict(Some(&[(marker.as_str(), &value.lower_deferred(state))]))
        }
        DeferredExpr::Op { op, args } => {
            let args: Vec<ValueRef> = args.iter().map(|a| a.lower_deferred(state)).collect();
            let refs: Vec<&ValueRef> = args.iter().collect();
            ValueRef::dict(Some(&[
                (DEFERRED_COND_KEY, &ValueRef::str(op)),
                ("args", &ValueRef::list(Some(&refs))),
            ]))
        }
    }
}

impl Context {
    /// The value of `v` from the known values, if its instance is named and
    /// the value is known.
    pub fn deferred_resolve(&mut self, v: &ValueRef) -> Option<ValueRef> {
        let Some(d) = v.as_deferred() else {
            return Some(v.clone());
        };
        match &d.expr {
            DeferredExpr::Ref { .. } => {
                let key = self.deferred.ref_key(&d)?;
                let json = self.deferred.known_map().get(&key)?.clone();
                ValueRef::from_json(self, &json.to_string()).ok()
            }
            DeferredExpr::Wrap { value, .. } => self.deferred_resolve(value),
            DeferredExpr::Template(parts) => {
                let mut out = String::new();
                for p in parts {
                    out.push_str(&self.deferred_resolve(p)?.to_string());
                }
                Some(ValueRef::str(&out))
            }
            DeferredExpr::Op { op, args } => {
                let args = args
                    .iter()
                    .map(|a| self.deferred_resolve(a))
                    .collect::<Option<Vec<_>>>()?;
                let b = match (op.as_str(), args.as_slice()) {
                    ("eq", [a, b]) => a.cmp_equal(b),
                    ("ne", [a, b]) => a.cmp_not_equal(b),
                    ("lt", [a, b]) => a.cmp_less_than(b),
                    ("le", [a, b]) => a.cmp_less_than_or_equal(b),
                    ("gt", [a, b]) => a.cmp_greater_than(b),
                    ("ge", [a, b]) => a.cmp_greater_than_or_equal(b),
                    ("contains", [container, item]) => item.r#in(container),
                    ("and", [a, b]) => a.is_truthy() && b.is_truthy(),
                    ("or", [a, b]) => a.is_truthy() || b.is_truthy(),
                    ("not", [a]) => !a.is_truthy(),
                    _ => return None,
                };
                Some(ValueRef::bool(b))
            }
        }
    }

    /// Whether `v` is concrete, or deferred with a known value.
    pub fn deferred_is_known(&mut self, v: &ValueRef) -> bool {
        self.deferred_resolve(v).is_some()
    }

    /// Gives unnamed instances below a top-level variable a name derived
    /// from `path`, and records the names of all instances.
    pub fn deferred_name_instances(&mut self, value: &ValueRef, path: &str) {
        if self.deferred.opts.kind_attr.is_empty() {
            return;
        }
        let kind_attr = self.deferred.opts.kind_attr.clone();
        let name_attr = self.deferred.opts.name_attr.clone();
        name_walk(self, value, path, &kind_attr, &name_attr);
    }
}

fn sanitize(path: &str) -> String {
    let s: String = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('-').to_string()
}

fn name_walk(ctx: &mut Context, value: &ValueRef, path: &str, kind_attr: &str, name_attr: &str) {
    let (children, instance) = match &*value.rc.borrow() {
        Value::list_value(l) => (
            l.values
                .iter()
                .enumerate()
                .map(|(i, v)| (format!("{path}-{i}"), v.clone()))
                .collect::<Vec<_>>(),
            None,
        ),
        Value::dict_value(d) => (dict_children(d, path), instance_of(d, kind_attr, name_attr)),
        Value::schema_value(s) => (
            dict_children(&s.config, path),
            instance_of(&s.config, kind_attr, name_attr),
        ),
        _ => return,
    };
    if let Some((oid, kind, name)) = instance {
        let name = match name {
            Some(n) => n,
            None => match ctx.deferred.names.get(&oid) {
                Some((_, n)) => n.clone(),
                None => sanitize(path),
            },
        };
        ctx.deferred
            .names
            .entry(oid)
            .or_insert((kind, name.clone()));
        let mut v = value.clone();
        if v.dict_get_value(name_attr)
            .is_none_or(|n| n.is_none_or_undefined())
        {
            v.dict_update_key_value(name_attr, ValueRef::str(&name));
        }
    }
    for (p, child) in children {
        name_walk(ctx, &child, &p, kind_attr, name_attr);
    }
}

fn dict_children(d: &DictValue, path: &str) -> Vec<(String, ValueRef)> {
    d.values
        .iter()
        .map(|(k, v)| (format!("{path}-{k}"), v.clone()))
        .collect()
}

/// `(oid, kind, explicit name)` of an instance to name.
fn instance_of(
    d: &DictValue,
    kind_attr: &str,
    name_attr: &str,
) -> Option<(u64, String, Option<String>)> {
    if d.oid.0 == 0 {
        return None;
    }
    let kind = d.values.get(kind_attr)?;
    if !kind.is_str() {
        return None;
    }
    let name = d
        .values
        .get(name_attr)
        .filter(|n| n.is_str())
        .map(|n| n.as_str());
    Some((d.oid.0, kind.as_str(), name))
}
