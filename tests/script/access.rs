//! The production parser is shared by access and default ACL command paths.
use super::{entry, AclEntry};
use std::assert_eq;
#[test]
fn acl_bits_reject_non_octal_signs_unicode_and_out_of_range_with_acl_specific_error() {
    for bits in ["", "8", "77", "1000", "-1", "+1", "0x7", "٧", "9999999999999999999"] {
        let text = std::format!("u:1001:{bits}");
        assert_eq!(entry(&text).unwrap_err(), "ACL bits must be 0..7", "{text}");
    }
}
#[test]
fn basic_named_and_mask_entries_parse_the_exact_bounded_literals() {
    assert_eq!(entry("u::0").unwrap(), AclEntry::Owner(0));
    assert_eq!(entry("g::07").unwrap(), AclEntry::GroupObj(7));
    assert_eq!(entry("o::4").unwrap(), AclEntry::Other(4));
    assert_eq!(entry("u:1001:6").unwrap(), AclEntry::User(1001, 6));
    assert_eq!(entry("g:5000:2").unwrap(), AclEntry::Group(5000, 2));
    assert_eq!(entry("m::0007").unwrap(), AclEntry::Mask(7));
}
