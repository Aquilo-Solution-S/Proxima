use std::fmt::Write as _;

#[must_use]
pub(crate) fn literal(vec: &[f32]) -> String {
    let mut out = String::with_capacity(vec.len().saturating_mul(8).saturating_add(2));
    out.push('[');
    for (idx, value) in vec.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        write!(&mut out, "{value}").expect("write to String is infallible");
    }
    out.push(']');
    out
}
