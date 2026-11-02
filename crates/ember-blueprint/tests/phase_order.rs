//! Compatibility is restricted to the Highcrown phase-order field.
use ember_blueprint::{convert_highcrown, parse, ErrorKind, Limits};

const SOURCE: &[u8] = include_bytes!("fixtures/highcrown-recorded.json");

/// Replace only the first phase order with an exact JSON token.
fn with_order(token: &str) -> Vec<u8> {
    let mut source: serde_json::Value = serde_json::from_slice(SOURCE).unwrap();
    source["content"]["guide"]["phases"][0]["num"] = serde_json::json!("PHASE_ORDER_FIXTURE");
    serde_json::to_string(&source)
        .unwrap()
        .replace("\"PHASE_ORDER_FIXTURE\"", token)
        .into_bytes()
}

#[test]
fn phase_order_integer_and_canonical_string_preserve_the_same_numeric_model() {
    for order in [0, 1, 42, u32::MAX] {
        let integer = with_order(&order.to_string());
        let string = with_order(&serde_json::to_string(&order.to_string()).unwrap());
        let numeric =
            convert_highcrown(&integer, "test:phase-order", 7, Limits::default()).unwrap();
        let textual = convert_highcrown(&string, "test:phase-order", 7, Limits::default()).unwrap();
        assert_eq!(numeric.document().stages, textual.document().stages);
        assert!(textual
            .document()
            .stages
            .iter()
            .any(|stage| stage.order == order));
        assert_ne!(
            numeric.revision().content_sha256,
            textual.revision().content_sha256
        );
        let mut normalized = numeric.document().clone();
        normalized.source.revision.content_sha256 = textual.revision().content_sha256.clone();
        assert_eq!(&normalized, textual.document());
        let exported: serde_json::Value =
            serde_json::from_slice(&textual.to_json().unwrap()).unwrap();
        assert!(exported["stages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|stage| stage["order"].is_u64()));
        assert_eq!(
            parse(&textual.to_json().unwrap(), Limits::default()).unwrap(),
            textual
        );
    }
}

#[test]
fn phase_order_rejects_other_json_types_and_noncanonical_or_out_of_range_strings() {
    let mut tokens: Vec<String> = [
        "-1",
        "-0",
        "1.5",
        "1.0",
        "1e0",
        "true",
        "false",
        "null",
        "[]",
        "{}",
        "4294967296",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    tokens.extend(
        [
            "",
            " ",
            " 1",
            "1 ",
            "+1",
            "-1",
            "-0",
            "1.0",
            "1e0",
            "00",
            "01",
            "4294967296",
            "11111111111",
            "١",
            "１２",
        ]
        .into_iter()
        .map(|value| serde_json::to_string(value).unwrap()),
    );
    for token in tokens {
        assert_eq!(
            convert_highcrown(
                &with_order(&token),
                "test:bad-phase-order",
                1,
                Limits::default()
            )
            .unwrap_err()
            .kind,
            ErrorKind::InvalidJson,
            "{token}"
        );
    }
}

#[test]
fn phase_order_compatibility_does_not_coerce_other_adapter_or_public_fields() {
    let mut source: serde_json::Value = serde_json::from_slice(SOURCE).unwrap();
    source["block_m"] = serde_json::json!("0.5");
    assert_eq!(
        convert_highcrown(
            &serde_json::to_vec(&source).unwrap(),
            "test:no-global-coercion",
            1,
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::InvalidJson
    );
    let model = convert_highcrown(SOURCE, "test:public-stage-order", 1, Limits::default()).unwrap();
    let mut public: serde_json::Value = serde_json::from_slice(&model.to_json().unwrap()).unwrap();
    public["stages"][0]["order"] = serde_json::json!("1");
    assert_eq!(
        parse(&serde_json::to_vec(&public).unwrap(), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidJson
    );
}
