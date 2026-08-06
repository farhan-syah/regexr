//! Parser unit tests.

use super::*;
use crate::error::ErrorKind;

#[test]
fn test_literal() {
    let ast = parse("abc").unwrap();
    assert!(matches!(ast.expr, Expr::Concat(_)));
}

#[test]
fn test_alternation() {
    let ast = parse("a|b|c").unwrap();
    if let Expr::Alt(alts) = ast.expr {
        assert_eq!(alts.len(), 3);
    } else {
        panic!("Expected Alt");
    }
}

#[test]
fn test_quantifiers() {
    let ast = parse("a*b+c?").unwrap();
    if let Expr::Concat(exprs) = ast.expr {
        assert!(matches!(exprs[0], Expr::Repeat(_)));
        assert!(matches!(exprs[1], Expr::Repeat(_)));
        assert!(matches!(exprs[2], Expr::Repeat(_)));
    } else {
        panic!("Expected Concat");
    }
}

#[test]
fn test_repetition_range() {
    let ast = parse("a{2,5}").unwrap();
    if let Expr::Repeat(rep) = ast.expr {
        assert_eq!(rep.min, 2);
        assert_eq!(rep.max, Some(5));
    } else {
        panic!("Expected Repeat");
    }
}

#[test]
fn test_character_class() {
    let ast = parse("[a-z]").unwrap();
    if let Expr::Class(cls) = ast.expr {
        assert!(!cls.negated);
        assert_eq!(cls.ranges.len(), 1);
        assert_eq!(cls.ranges[0].start, 'a');
        assert_eq!(cls.ranges[0].end, 'z');
    } else {
        panic!("Expected Class");
    }
}

#[test]
fn test_negated_class() {
    let ast = parse("[^0-9]").unwrap();
    if let Expr::Class(cls) = ast.expr {
        assert!(cls.negated);
    } else {
        panic!("Expected Class");
    }
}

#[test]
fn test_capturing_group() {
    let ast = parse("(abc)").unwrap();
    if let Expr::Group(g) = ast.expr {
        assert!(matches!(g.kind, GroupKind::Capturing(1)));
    } else {
        panic!("Expected Group");
    }
}

#[test]
fn test_non_capturing_group() {
    let ast = parse("(?:abc)").unwrap();
    if let Expr::Group(g) = ast.expr {
        assert!(matches!(g.kind, GroupKind::NonCapturing));
    } else {
        panic!("Expected Group");
    }
}

#[test]
fn test_lookahead() {
    let ast = parse("(?=abc)").unwrap();
    if let Expr::Lookaround(la) = ast.expr {
        assert!(matches!(la.kind, LookaroundKind::PositiveLookahead));
    } else {
        panic!("Expected Lookaround");
    }
}

#[test]
fn test_escape_sequences() {
    let ast = parse(r"\d\w\s").unwrap();
    if let Expr::Concat(exprs) = ast.expr {
        assert_eq!(exprs.len(), 3);
        assert!(matches!(exprs[0], Expr::PerlClass(PerlClassKind::Digit)));
        assert!(matches!(exprs[1], Expr::PerlClass(PerlClassKind::Word)));
        assert!(matches!(
            exprs[2],
            Expr::PerlClass(PerlClassKind::Whitespace)
        ));
    } else {
        panic!("Expected Concat");
    }
}

#[test]
fn test_anchors() {
    let ast = parse(r"^\w+$").unwrap();
    if let Expr::Concat(exprs) = ast.expr {
        assert!(matches!(exprs[0], Expr::Anchor(Anchor::StartOfString)));
        assert!(matches!(exprs[2], Expr::Anchor(Anchor::EndOfString)));
    } else {
        panic!("Expected Concat");
    }
}

#[test]
fn test_error_unmatched_paren() {
    let err = parse("(abc").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::UnmatchedOpenParen));
}

#[test]
fn test_error_quantifier_on_nothing() {
    let err = parse("*abc").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::RepetitionOnNothing));
}

#[test]
fn test_error_nested_quantifier() {
    let err = parse("a**").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::NestedQuantifier));
}
