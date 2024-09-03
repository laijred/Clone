use crate::{
    update::new::KvReaderFieldId, FieldId, FieldsIdsMap, Index, InternalError,
    LocalizedAttributesRule, Result, MAX_POSITION_PER_ATTRIBUTE, MAX_WORD_LENGTH,
};
use charabia::{SeparatorKind, Token, TokenKind, Tokenizer, TokenizerBuilder};
use heed::RoTxn;
use serde_json::Value;
use std::collections::HashMap;

pub struct DocumentTokenizer<'a> {
    pub tokenizer: &'a Tokenizer<'a>,
    pub searchable_attributes: Option<&'a [&'a str]>,
    pub localized_attributes_rules: &'a [LocalizedAttributesRule],
    pub max_positions_per_attributes: u32,
}

impl<'a> DocumentTokenizer<'a> {
    pub fn tokenize_document(
        &self,
        obkv: &KvReaderFieldId,
        field_id_map: &FieldsIdsMap,
        token_fn: &mut impl FnMut(FieldId, u16, &str),
    ) -> Result<()> {
        let mut field_position = HashMap::new();
        for (field_id, field_bytes) in obkv {
            let Some(field_name) = field_id_map.name(field_id) else {
                unreachable!("field id not found in field id map");
            };

            let mut tokenize_field = |name: &str, value: &Value| {
                let Some(field_id) = field_id_map.id(name) else {
                    unreachable!("field name not found in field id map");
                };

                let position =
                    field_position.entry(field_id).and_modify(|counter| *counter += 8).or_insert(0);
                if *position as u32 >= self.max_positions_per_attributes {
                    return;
                }

                match value {
                    Value::Number(n) => {
                        let token = n.to_string();
                        if let Ok(position) = (*position).try_into() {
                            token_fn(field_id, position, token.as_str());
                        }
                    }
                    Value::String(text) => {
                        // create an iterator of token with their positions.
                        let locales = self
                            .localized_attributes_rules
                            .iter()
                            .find(|rule| rule.match_str(field_name))
                            .map(|rule| rule.locales());
                        let tokens = process_tokens(
                            *position,
                            self.tokenizer.tokenize_with_allow_list(text.as_str(), locales),
                        )
                        .take_while(|(p, _)| (*p as u32) < self.max_positions_per_attributes);

                        for (index, token) in tokens {
                            // keep a word only if it is not empty and fit in a LMDB key.
                            let token = token.lemma().trim();
                            if !token.is_empty() && token.len() <= MAX_WORD_LENGTH {
                                *position = index;
                                if let Ok(position) = (*position).try_into() {
                                    token_fn(field_id, position, token);
                                }
                            }
                        }
                    }
                    _ => (),
                }
            };

            // if the current field is searchable or contains a searchable attribute
            if self.searchable_attributes.map_or(true, |attributes| {
                attributes.iter().any(|name| perm_json_p::contained_in(name, field_name))
            }) {
                // parse json.
                match serde_json::from_slice(field_bytes).map_err(InternalError::SerdeJson)? {
                    Value::Object(object) => perm_json_p::seek_leaf_values_in_object(
                        &object,
                        self.searchable_attributes.as_deref(),
                        &field_name,
                        &mut tokenize_field,
                    ),
                    Value::Array(array) => perm_json_p::seek_leaf_values_in_array(
                        &array,
                        self.searchable_attributes.as_deref(),
                        &field_name,
                        &mut tokenize_field,
                    ),
                    value => tokenize_field(&field_name, &value),
                }
            }
        }
        Ok(())
    }
}

/// take an iterator on tokens and compute their relative position depending on separator kinds
/// if it's an `Hard` separator we add an additional relative proximity of 8 between words,
/// else we keep the standard proximity of 1 between words.
fn process_tokens<'a>(
    start_offset: usize,
    tokens: impl Iterator<Item = Token<'a>>,
) -> impl Iterator<Item = (usize, Token<'a>)> {
    tokens
        .skip_while(|token| token.is_separator())
        .scan((start_offset, None), |(offset, prev_kind), mut token| {
            match token.kind {
                TokenKind::Word | TokenKind::StopWord if !token.lemma().is_empty() => {
                    *offset += match *prev_kind {
                        Some(TokenKind::Separator(SeparatorKind::Hard)) => 8,
                        Some(_) => 1,
                        None => 0,
                    };
                    *prev_kind = Some(token.kind)
                }
                TokenKind::Separator(SeparatorKind::Hard) => {
                    *prev_kind = Some(token.kind);
                }
                TokenKind::Separator(SeparatorKind::Soft)
                    if *prev_kind != Some(TokenKind::Separator(SeparatorKind::Hard)) =>
                {
                    *prev_kind = Some(token.kind);
                }
                _ => token.kind = TokenKind::Unknown,
            }
            Some((*offset, token))
        })
        .filter(|(_, t)| t.is_word())
}

/// TODO move in permissive json pointer
mod perm_json_p {
    use serde_json::{Map, Value};
    const SPLIT_SYMBOL: char = '.';

    /// Returns `true` if the `selector` match the `key`.
    ///
    /// ```text
    /// Example:
    /// `animaux`           match `animaux`
    /// `animaux.chien`     match `animaux`
    /// `animaux.chien`     match `animaux`
    /// `animaux.chien.nom` match `animaux`
    /// `animaux.chien.nom` match `animaux.chien`
    /// -----------------------------------------
    /// `animaux`    doesn't match `animaux.chien`
    /// `animaux.`   doesn't match `animaux`
    /// `animaux.ch` doesn't match `animaux.chien`
    /// `animau`     doesn't match `animaux`
    /// ```
    pub fn contained_in(selector: &str, key: &str) -> bool {
        selector.starts_with(key)
            && selector[key.len()..].chars().next().map(|c| c == SPLIT_SYMBOL).unwrap_or(true)
    }

    pub fn seek_leaf_values<'a>(
        value: &Map<String, Value>,
        selectors: impl IntoIterator<Item = &'a str>,
        seeker: &mut impl FnMut(&str, &Value),
    ) {
        let selectors: Vec<_> = selectors.into_iter().collect();
        seek_leaf_values_in_object(value, Some(&selectors), "", seeker);
    }

    pub fn seek_leaf_values_in_object(
        value: &Map<String, Value>,
        selectors: Option<&[&str]>,
        base_key: &str,
        seeker: &mut impl FnMut(&str, &Value),
    ) {
        for (key, value) in value.iter() {
            let base_key = if base_key.is_empty() {
                key.to_string()
            } else {
                format!("{}{}{}", base_key, SPLIT_SYMBOL, key)
            };

            // here if the user only specified `doggo` we need to iterate in all the fields of `doggo`
            // so we check the contained_in on both side
            let should_continue = selectors.map_or(true, |selectors| {
                selectors.iter().any(|selector| {
                    contained_in(selector, &base_key) || contained_in(&base_key, selector)
                })
            });

            if should_continue {
                match value {
                    Value::Object(object) => {
                        seek_leaf_values_in_object(object, selectors, &base_key, seeker)
                    }
                    Value::Array(array) => {
                        seek_leaf_values_in_array(array, selectors, &base_key, seeker)
                    }
                    value => seeker(&base_key, value),
                }
            }
        }
    }

    pub fn seek_leaf_values_in_array(
        values: &[Value],
        selectors: Option<&[&str]>,
        base_key: &str,
        seeker: &mut impl FnMut(&str, &Value),
    ) {
        for value in values {
            match value {
                Value::Object(object) => {
                    seek_leaf_values_in_object(object, selectors, base_key, seeker)
                }
                Value::Array(array) => {
                    seek_leaf_values_in_array(array, selectors, base_key, seeker)
                }
                value => seeker(base_key, value),
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use charabia::TokenizerBuilder;
    use meili_snap::snapshot;
    use obkv::KvReader;
    use serde_json::json;
    #[test]
    fn test_tokenize_document() {
        let mut fields_ids_map = FieldsIdsMap::new();

        let field_1 = json!({
                "name": "doggo",
                "age": 10,
        });

        let field_2 = json!({
                "catto": {
                    "name": "pesti",
                    "age": 23,
                }
        });

        let field_3 = json!(["doggo", "catto"]);

        let mut obkv = obkv::KvWriter::memory();
        let field_1_id = fields_ids_map.insert("doggo").unwrap();
        let field_1 = serde_json::to_string(&field_1).unwrap();
        obkv.insert(field_1_id, field_1.as_bytes()).unwrap();
        let field_2_id = fields_ids_map.insert("catto").unwrap();
        let field_2 = serde_json::to_string(&field_2).unwrap();
        obkv.insert(field_2_id, field_2.as_bytes()).unwrap();
        let field_3_id = fields_ids_map.insert("doggo.name").unwrap();
        let field_3 = serde_json::to_string(&field_3).unwrap();
        obkv.insert(field_3_id, field_3.as_bytes()).unwrap();
        let value = obkv.into_inner().unwrap();
        let obkv = KvReader::from_slice(value.as_slice());

        fields_ids_map.insert("doggo.age");
        fields_ids_map.insert("catto.catto.name");
        fields_ids_map.insert("catto.catto.age");

        let mut tb = TokenizerBuilder::default();
        let document_tokenizer = DocumentTokenizer {
            tokenizer: &tb.build(),
            searchable_attributes: None,
            localized_attributes_rules: &[],
            max_positions_per_attributes: 1000,
        };

        let mut words = std::collections::BTreeMap::new();
        document_tokenizer
            .tokenize_document(obkv, &fields_ids_map, &mut |fid, pos, word| {
                words.insert([fid, pos], word.to_string());
            })
            .unwrap();

        snapshot!(format!("{:#?}", words), @r###"
        {
            [
                2,
                0,
            ]: "doggo",
            [
                2,
                8,
            ]: "doggo",
            [
                2,
                16,
            ]: "catto",
            [
                3,
                0,
            ]: "10",
            [
                4,
                0,
            ]: "pesti",
            [
                5,
                0,
            ]: "23",
        }
        "###);
    }
}