use orange_beam::*;

#[test]
fn bluetooth_queries_are_long_zero_padded_and_big_endian() {
    let request = feature_request(1, 0x1b04);
    assert_eq!(&request.bytes()[..7], &[0x11, 1, 0, 0x0a, 0x1b, 4, 0]);
    assert_eq!(request.bytes().len(), 20);
    assert!(request.bytes()[7..].iter().all(|b| *b == 0));
}

#[test]
fn malformed_reports_are_rejected_without_panics() {
    for id in 0..=255 {
        for len in 0..=65 {
            let mut data = vec![0; len];
            if len > 0 {
                data[0] = id;
            }
            let valid = (id == 0x10 && len == 7) || (id == 0x11 && len == 20);
            assert_eq!(Report::parse(&data).is_ok(), valid);
        }
    }
}

#[test]
fn rejects_values_that_would_corrupt_the_header() {
    assert_eq!(
        Report::request(1, 1, 16, 1, &[]),
        Err(ProtocolError::InvalidFunction)
    );
    for id in [0, 16, 255] {
        assert_eq!(
            Report::request(1, 1, 0, id, &[]),
            Err(ProtocolError::InvalidSoftwareId)
        );
    }
    assert_eq!(
        Report::request(1, 1, 0, 1, &[0; 17]),
        Err(ProtocolError::PayloadTooLong)
    );
}

#[test]
fn ignores_notifications_other_apps_and_other_devices() {
    let request = feature_request(1, 0x1000);
    for (device, feature, address) in [
        (2, 0, 0x0a),
        (1, 1, 0x0a),
        (1, 0, 0),
        (1, 0, 0x0b),
        (1, 0, 0x1a),
    ] {
        let reply = Report::parse(&[0x10, device, feature, address, 7, 0, 0]).unwrap();
        assert_eq!(reply.response_to(&request), ResponseKind::Unrelated);
    }
}

#[test]
fn discovers_indices_instead_of_hardcoding_them() {
    let request = feature_request(1, 0x1000);
    for index in [0, 1, 7, 42] {
        let reply = Report::parse(&[0x10, 1, 0, 0x0a, index, 0, 0]).unwrap();
        assert_eq!(
            feature_index(&reply, &request).unwrap(),
            if index == 0 { None } else { Some(index) }
        );
    }
}

#[test]
fn recognizes_correlated_errors_and_ignores_unrelated_errors() {
    let request = feature_request(1, 0x1000);
    for (id, len, marker) in [(0x10, 7, 0x8f), (0x11, 20, 0xfe)] {
        let mut bytes = vec![0; len];
        bytes[..7].copy_from_slice(&[id, 1, marker, 0, 0x0a, 6, 0]);
        let reply = Report::parse(&bytes).unwrap();
        assert_eq!(
            feature_index(&reply, &request),
            Err(ProtocolError::Device {
                legacy: marker == 0x8f,
                code: 6
            })
        );
        bytes[4] = 0x0b;
        assert_eq!(
            Report::parse(&bytes).unwrap().response_to(&request),
            ResponseKind::Unrelated
        );
    }
}

#[test]
fn shared_bolt_receivers_and_other_logitech_products_are_not_claimed() {
    assert!(is_spotlight(0x046d, 0xc53e));
    assert!(is_spotlight(0x046d, 0xb503));
    assert!(!is_spotlight(0x046d, 0xc548));
    assert!(!is_spotlight(0x046d, 0xc52b));
    assert!(!is_spotlight(0x1234, 0xc53e));
}
