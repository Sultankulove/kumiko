//! How long cosmic-text takes to set up fonts: the whole system font database (what iced does)
//! against a database with only the editor's monospace font (what our editor widget could do).

use std::time::Instant;

use cosmic_text::fontdb::Database;
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

fn shape_line(fonts: &mut FontSystem) -> f64 {
    let started = Instant::now();
    let mut buffer = Buffer::new(fonts, Metrics::new(14.0, 20.0));
    let attrs = Attrs::new().family(Family::Monospace);
    buffer.set_text(
        fonts,
        "public final class Sample { int x = 42; }",
        &attrs,
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(fonts, false);
    started.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let started = Instant::now();
    let mut system = FontSystem::new();
    let system_ms = started.elapsed().as_secs_f64() * 1000.0;
    let faces = system.db().len();
    let system_shape = shape_line(&mut system);

    let monospace = if cfg!(windows) {
        "C:\\Windows\\Fonts\\consola.ttf"
    } else {
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
    };
    let started = Instant::now();
    let mut db = Database::new();
    let loaded = db.load_font_file(monospace).is_ok();
    db.set_monospace_family(if cfg!(windows) {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    });
    let mut narrow = FontSystem::new_with_locale_and_db("en-US".into(), db);
    let narrow_ms = started.elapsed().as_secs_f64() * 1000.0;
    let narrow_shape = shape_line(&mut narrow);

    println!(
        "{{\"system_fonts_ms\":{system_ms:.1},\"faces\":{faces},\"first_shape_ms\":{system_shape:.1},\"narrow_ms\":{narrow_ms:.1},\"narrow_loaded\":{loaded},\"narrow_first_shape_ms\":{narrow_shape:.1}}}"
    );
}
