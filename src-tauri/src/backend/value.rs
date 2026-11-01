//! Firestore's typed-`Value` wire format <-> `serde_json`.
//!
//! The REST API does not take plain JSON. Every field is wrapped in a
//! one-key object naming its type:
//!
//! ```json
//! { "fields": { "amount": { "doubleValue": 1008.0 },
//!               "item":   { "stringValue": "Titanium" } } }
//! ```
//!
//! Two traps live here, both of which produce data the web app displays
//! wrongly rather than an error:
//!
//! 1. **`integerValue` is a JSON *string***, not a number. Encoding it as a
//!    number is rejected; decoding it as one silently yields a string.
//! 2. **Floats that happen to be whole must stay doubles.** `buyPrice` of
//!    exactly 200.0 encoded as `integerValue` comes back as an integer, and a
//!    later weighted average then mixes types.

use serde_json::{Map, Value};

/// Encode one JSON value into Firestore's tagged form.
pub fn to_firestore(v: &Value) -> Value {
    match v {
        Value::Null => serde_json::json!({ "nullValue": null }),
        Value::Bool(b) => serde_json::json!({ "booleanValue": b }),
        Value::Number(n) => {
            // Only values that are *integers in the JSON sense* become
            // integerValue. A Number built from an f64 — which is every
            // numeric field in our model — reports as_i64() == None and stays
            // a double, which is what we want.
            if let Some(i) = n.as_i64() {
                serde_json::json!({ "integerValue": i.to_string() })
            } else {
                serde_json::json!({ "doubleValue": n.as_f64().unwrap_or(0.0) })
            }
        }
        Value::String(s) => serde_json::json!({ "stringValue": s }),
        Value::Array(items) => {
            let values: Vec<Value> = items.iter().map(to_firestore).collect();
            serde_json::json!({ "arrayValue": { "values": values } })
        }
        Value::Object(map) => {
            serde_json::json!({ "mapValue": { "fields": encode_fields(map) } })
        }
    }
}

/// Encode a flat record into a Firestore `fields` object.
pub fn encode_fields(map: &Map<String, Value>) -> Map<String, Value> {
    map.iter()
        .map(|(k, v)| (k.clone(), to_firestore(v)))
        .collect()
}

/// Decode one Firestore-tagged value back to plain JSON.
pub fn from_firestore(v: &Value) -> Value {
    let Some(obj) = v.as_object() else {
        return Value::Null;
    };
    if obj.contains_key("nullValue") {
        return Value::Null;
    }
    if let Some(b) = obj.get("booleanValue").and_then(Value::as_bool) {
        return Value::Bool(b);
    }
    if let Some(s) = obj.get("integerValue").and_then(Value::as_str) {
        // Arrives as a string; put it back as a number.
        if let Ok(i) = s.parse::<i64>() {
            return Value::Number(i.into());
        }
    }
    if let Some(n) = obj.get("doubleValue").and_then(Value::as_f64) {
        return serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number);
    }
    if let Some(s) = obj.get("stringValue").and_then(Value::as_str) {
        return Value::String(s.to_string());
    }
    if let Some(s) = obj.get("timestampValue").and_then(Value::as_str) {
        return Value::String(s.to_string());
    }
    if let Some(arr) = obj.get("arrayValue") {
        let items = arr
            .get("values")
            .and_then(Value::as_array)
            .map(|vs| vs.iter().map(from_firestore).collect())
            .unwrap_or_default();
        return Value::Array(items);
    }
    if let Some(m) = obj.get("mapValue") {
        if let Some(fields) = m.get("fields").and_then(Value::as_object) {
            return Value::Object(decode_fields(fields));
        }
        return Value::Object(Map::new());
    }
    Value::Null
}

/// Decode a Firestore `fields` object into a flat record.
pub fn decode_fields(fields: &Map<String, Value>) -> Map<String, Value> {
    fields
        .iter()
        .map(|(k, v)| (k.clone(), from_firestore(v)))
        .collect()
}

/// The document id is the last path segment of a Firestore `name`.
pub fn id_from_name(name: &str) -> Option<&str> {
    name.rsplit('/').next().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(v: Value) -> Value {
        from_firestore(&to_firestore(&v))
    }

    #[test]
    fn integer_value_is_encoded_as_a_string() {
        let enc = to_firestore(&json!(42));
        assert_eq!(enc, json!({ "integerValue": "42" }));
        assert!(
            enc["integerValue"].is_string(),
            "Firestore rejects a numeric integerValue"
        );
    }

    /// The precision trap: a price that happens to be whole must not become an
    /// integer on the way out.
    #[test]
    fn whole_floats_stay_doubles() {
        let price = json!(200.0_f64);
        let enc = to_firestore(&price);
        assert!(
            enc.get("doubleValue").is_some(),
            "200.0 must encode as doubleValue, got {enc}"
        );
        assert_eq!(round_trip(price).as_f64(), Some(200.0));
    }

    #[test]
    fn unrounded_prices_survive() {
        for p in [188.9409_f64, 1.889409, 495.5, 1212.625, 799.0917999999999] {
            let back = round_trip(json!(p)).as_f64().unwrap();
            assert_eq!(back, p, "price changed crossing the wire");
        }
    }

    #[test]
    fn strings_bools_and_null_round_trip() {
        assert_eq!(round_trip(json!("Titanium")), json!("Titanium"));
        assert_eq!(round_trip(json!(true)), json!(true));
        assert_eq!(round_trip(json!(null)), json!(null));
    }

    /// U+03BC must survive the encoding unchanged.
    #[test]
    fn mu_scu_survives_the_wire() {
        let v = json!("\u{03BC}SCU");
        let enc = to_firestore(&v);
        assert_eq!(enc["stringValue"], json!("\u{03BC}SCU"));
        assert_eq!(round_trip(v).as_str(), Some("\u{03BC}SCU"));
    }

    #[test]
    fn nested_maps_and_arrays_round_trip() {
        let v = json!({ "a": [1, 2.5, "x"], "b": { "c": true } });
        let Value::Object(map) = &v else { unreachable!() };
        let decoded = Value::Object(decode_fields(&encode_fields(map)));
        assert_eq!(decoded, v);
    }

    /// A whole asset document, as it actually goes over the wire.
    #[test]
    fn asset_record_round_trips_through_fields() {
        use crate::model::{AssetRecord, SOURCE};
        use crate::units::CargoUnit;

        let a = AssetRecord {
            id: "4ed7a396-7398-590d-be1c-d3a012c0dcb6".into(),
            item: "Titanium".into(),
            amount: 1008.0,
            unit: Some(CargoUnit::Scu),
            buy_price: 188.9409,
            location: "Area18".into(),
            quality: None,
            created_at: "2026-11-01T09:11:29.754Z".into(),
            logged_by: Some("m1nd".into()),
            source: Some(SOURCE.into()),
            uex_buy_id: None,
        };

        let plain = serde_json::to_value(&a).unwrap();
        let Value::Object(map) = &plain else { unreachable!() };
        let wire = encode_fields(map);

        // Spot-check the shape the server will actually see.
        assert_eq!(wire["item"], json!({ "stringValue": "Titanium" }));
        assert_eq!(wire["amount"], json!({ "doubleValue": 1008.0 }));
        assert!(!wire.contains_key("quality"), "absent stays absent");

        let back: AssetRecord =
            serde_json::from_value(Value::Object(decode_fields(&wire))).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.buy_price, 188.9409);
        assert_eq!(back.quality, None);
    }

    #[test]
    fn id_is_the_last_path_segment() {
        assert_eq!(
            id_from_name("projects/p/databases/(default)/documents/assets/abc-123"),
            Some("abc-123")
        );
        assert_eq!(id_from_name(""), None);
    }

    /// Unknown tags decode to null rather than panicking — a forward-compat
    /// guard, since Firestore has types we never write (geoPoint, bytes, ref).
    #[test]
    fn unknown_tags_decode_to_null() {
        assert_eq!(from_firestore(&json!({ "geoPointValue": {} })), json!(null));
        assert_eq!(from_firestore(&json!("not an object")), json!(null));
    }
}
