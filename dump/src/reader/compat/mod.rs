// pub mod v2;
// pub mod v3;
// pub mod v4;

// pub mod v4_to_v5;
pub mod v5_to_v6;

pub struct Compat<From: ?Sized> {
    from: Box<From>,
}

/// Parses the v1 version of the Asc ranking rules `asc(price)`and returns the field name.
pub fn asc_ranking_rule(text: &str) -> Option<&str> {
    text.split_once("asc(")
        .and_then(|(_, tail)| tail.rsplit_once(')'))
        .map(|(field, _)| field)
}

/// Parses the v1 version of the Desc ranking rules `desc(price)`and returns the field name.
pub fn desc_ranking_rule(text: &str) -> Option<&str> {
    text.split_once("desc(")
        .and_then(|(_, tail)| tail.rsplit_once(')'))
        .map(|(field, _)| field)
}
