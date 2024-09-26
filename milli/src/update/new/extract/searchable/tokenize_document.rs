use std::collections::HashMap;

use charabia::{SeparatorKind, Token, TokenKind, Tokenizer, TokenizerBuilder};
use serde_json::Value;

use crate::proximity::MAX_DISTANCE;
use crate::update::new::document::Document;
use crate::update::new::extract::perm_json_p::{
    seek_leaf_values_in_array, seek_leaf_values_in_object, select_field,
};
use crate::{
    FieldId, GlobalFieldsIdsMap, InternalError, LocalizedAttributesRule, Result, UserError,
    MAX_WORD_LENGTH,
};

pub struct DocumentTokenizer<'a> {
    pub tokenizer: &'a Tokenizer<'a>,
    pub attribute_to_extract: Option<&'a [&'a str]>,
    pub attribute_to_skip: &'a [&'a str],
    pub localized_attributes_rules: &'a [LocalizedAttributesRule],
    pub max_positions_per_attributes: u32,
}

impl<'a> DocumentTokenizer<'a> {
    pub fn tokenize_document<'d>(
        &self,
        document: &'d impl Document<'d>,
        field_id_map: &mut GlobalFieldsIdsMap,
        token_fn: &mut impl FnMut(&str, FieldId, u16, &str) -> Result<()>,
    ) -> Result<()> {
        let mut field_position = HashMap::new();

        for (field_name, value) in document.iter_top_level_fields() {
            let mut tokenize_field = |name: &str, value: &Value| {
                let Some(field_id) = field_id_map.id_or_insert(name) else {
                    return Err(UserError::AttributeLimitReached.into());
                };

                let position = field_position
                    .entry(field_id)
                    .and_modify(|counter| *counter += MAX_DISTANCE)
                    .or_insert(0);
                if *position as u32 >= self.max_positions_per_attributes {
                    return Ok(());
                }

                match value {
                    Value::Number(n) => {
                        let token = n.to_string();
                        if let Ok(position) = (*position).try_into() {
                            token_fn(name, field_id, position, token.as_str())?;
                        }

                        Ok(())
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
                                    token_fn(name, field_id, position, token)?;
                                }
                            }
                        }

                        Ok(())
                    }
                    _ => Ok(()),
                }
            };

            // if the current field is searchable or contains a searchable attribute
            if select_field(field_name, self.attribute_to_extract, self.attribute_to_skip) {
                // parse json.
                match serde_json::to_value(value).map_err(InternalError::SerdeJson)? {
                    Value::Object(object) => seek_leaf_values_in_object(
                        &object,
                        self.attribute_to_extract,
                        self.attribute_to_skip,
                        field_name,
                        &mut tokenize_field,
                    )?,
                    Value::Array(array) => seek_leaf_values_in_array(
                        &array,
                        self.attribute_to_extract,
                        self.attribute_to_skip,
                        field_name,
                        &mut tokenize_field,
                    )?,
                    value => tokenize_field(field_name, &value)?,
                }
            }
        }

        Ok(())
    }
}

/// take an iterator on tokens and compute their relative position depending on separator kinds
/// if it's an `Hard` separator we add an additional relative proximity of MAX_DISTANCE between words,
/// else we keep the standard proximity of 1 between words.
fn process_tokens<'a>(
    start_offset: u32,
    tokens: impl Iterator<Item = Token<'a>>,
) -> impl Iterator<Item = (u32, Token<'a>)> {
    tokens
        .skip_while(|token| token.is_separator())
        .scan((start_offset, None), |(offset, prev_kind), mut token| {
            match token.kind {
                TokenKind::Word | TokenKind::StopWord if !token.lemma().is_empty() => {
                    *offset += match *prev_kind {
                        Some(TokenKind::Separator(SeparatorKind::Hard)) => MAX_DISTANCE,
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

/// Factorize tokenizer building.
pub fn tokenizer_builder<'a>(
    stop_words: Option<&'a fst::Set<&'a [u8]>>,
    allowed_separators: Option<&'a [&str]>,
    dictionary: Option<&'a [&str]>,
) -> TokenizerBuilder<'a, &'a [u8]> {
    let mut tokenizer_builder = TokenizerBuilder::new();
    if let Some(stop_words) = stop_words {
        tokenizer_builder.stop_words(stop_words);
    }
    if let Some(dictionary) = dictionary {
        tokenizer_builder.words_dict(dictionary);
    }
    if let Some(separators) = allowed_separators {
        tokenizer_builder.separators(separators);
    }

    tokenizer_builder
}

#[cfg(test)]
mod test {
    use charabia::TokenizerBuilder;
    use meili_snap::snapshot;
    use obkv::KvReader;
    use serde_json::json;

    use super::*;
    use crate::update::new::TopLevelMap;
    use crate::FieldsIdsMap;

    #[test]
    fn test_tokenize_document() {
        let mut fields_ids_map = FieldsIdsMap::new();

        let document = json!({
            "doggo": {                "name": "doggo",
            "age": 10,},
            "catto": {
                "catto": {
                    "name": "pesti",
                    "age": 23,
                }
            },
            "doggo.name": ["doggo", "catto"],
            "not-me": "UNSEARCHABLE",
            "me-nether": {"nope": "unsearchable"}
        });

        let _field_1_id = fields_ids_map.insert("doggo").unwrap();
        let _field_2_id = fields_ids_map.insert("catto").unwrap();
        let _field_3_id = fields_ids_map.insert("doggo.name").unwrap();
        let _field_4_id = fields_ids_map.insert("not-me").unwrap();
        let _field_5_id = fields_ids_map.insert("me-nether").unwrap();

        let mut tb = TokenizerBuilder::default();
        let document_tokenizer = DocumentTokenizer {
            tokenizer: &tb.build(),
            attribute_to_extract: None,
            attribute_to_skip: &["not-me", "me-nether.nope"],
            localized_attributes_rules: &[],
            max_positions_per_attributes: 1000,
        };

        let fields_ids_map_lock = std::sync::RwLock::new(fields_ids_map);
        let mut global_fields_ids_map = GlobalFieldsIdsMap::new(&fields_ids_map_lock);

        let mut words = std::collections::BTreeMap::new();

        let document = document.to_string();

        let document: TopLevelMap = serde_json::from_str(&document).unwrap();

        document_tokenizer
            .tokenize_document(
                &document,
                &mut global_fields_ids_map,
                &mut |_fname, fid, pos, word| {
                    words.insert([fid, pos], word.to_string());
                    Ok(())
                },
            )
            .unwrap();

        snapshot!(format!("{:#?}", words), @r###"
        {
            [
                2,
                0,
            ]: "doggo",
            [
                2,
                MAX_DISTANCE,
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
