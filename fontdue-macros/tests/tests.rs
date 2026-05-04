use fontdue::FontRepr;
use fontdue_macros::fontdue_font_from_file;

#[test]
fn foo() {
    fontdue_font_from_file!(FontdueRobotoRegular, "../../dev/resources/fonts/Roboto-Regular.ttf", scale: 32);
}
