//! Deterministic offscreen checks of the same NSView drawing used by live panels.
//! Synthetic imagery only: no desktop capture or screen permission is needed.
use super::capture::tile_bounds;
use super::countdown::{AdjustHint, CountdownLook};
use super::*;
use objc2_core_image::{CIContext, CIImage};
use std::path::Path;

fn bitmap() -> Result<Retained<NSBitmapImageRep>, Box<dyn std::error::Error>> {
    unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(NSBitmapImageRep::alloc(), std::ptr::null_mut(), 800, 450, 8, 4, true, false, NSDeviceRGBColorSpace, 0, 0).ok_or_else(|| "Cannot allocate QA bitmap".into())
    }
}
fn context(bitmap: &NSBitmapImageRep) -> Result<(), Box<dyn std::error::Error>> {
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(bitmap)
        .ok_or("Cannot create graphics context")?;
    NSGraphicsContext::setCurrentContext(Some(&context));
    Ok(())
}
fn image(bitmap: &NSBitmapImageRep) -> Result<Retained<NSImage>, Box<dyn std::error::Error>> {
    let cg_image = bitmap.CGImage().ok_or("Missing CGImage")?;
    Ok(NSImage::initWithCGImage_size(
        NSImage::alloc(),
        &cg_image,
        NSSize::new(800.0, 450.0),
    ))
}
fn bytes(bitmap: &NSBitmapImageRep) -> Vec<u8> {
    let len = (bitmap.bytesPerRow() * bitmap.pixelsHigh()) as usize;
    // SAFETY: the rep owns a contiguous buffer of bytesPerRow * height bytes.
    unsafe { std::slice::from_raw_parts(bitmap.bitmapData(), len) }.to_vec()
}
fn write(bitmap: &NSBitmapImageRep, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or("PNG encoding failed")?;
    std::fs::write(path, png.to_vec())?;
    Ok(())
}

pub fn run(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mtm = MainThreadMarker::new().ok_or("Render QA must run on main")?;
    let _app = NSApplication::sharedApplication(mtm);
    std::fs::create_dir_all(directory)?;
    let size = NSSize::new(800.0, 450.0);
    let bounds = NSRect::new(NSPoint::ZERO, size);
    let background = bitmap()?;
    context(&background)?;
    NSColor::colorWithSRGBRed_green_blue_alpha(0.96, 0.97, 0.99, 1.0).setFill();
    NSRectFillUsingOperation(bounds, NSCompositingOperation::Copy);
    for row in 0..5 {
        for column in 0..9 {
            let x = 40.0 + column as f64 * 80.0;
            let y = 30.0 + row as f64 * 80.0;
            let color = if (row + column) % 2 == 0 {
                (0.16, 0.41, 0.80)
            } else {
                (0.95, 0.61, 0.17)
            };
            NSColor::colorWithSRGBRed_green_blue_alpha(color.0, color.1, color.2, 1.0).setFill();
            NSBezierPath::bezierPathWithRect(NSRect::new(
                NSPoint::new(x, y),
                NSSize::new(45.0, 35.0),
            ))
            .fill();
            NSColor::colorWithSRGBRed_green_blue_alpha(0.12, 0.17, 0.24, 1.0).setFill();
            NSBezierPath::bezierPathWithRect(NSRect::new(
                NSPoint::new(x, y + 43.0),
                NSSize::new(55.0, 3.0),
            ))
            .fill();
            NSBezierPath::bezierPathWithRect(NSRect::new(
                NSPoint::new(x, y + 50.0),
                NSSize::new(35.0, 3.0),
            ))
            .fill();
        }
    }
    write(&background, &directory.join("synthetic-source.png"))?;
    let source = image(&background)?;
    for (name, effect) in [
        ("spotlight", Effect::Spotlight),
        ("laser", Effect::Laser),
        ("magnify", Effect::Magnify),
        ("box", Effect::Box),
    ] {
        let view = OverlayView::alloc(mtm).set_ivars(ViewData::default());
        let view: Retained<OverlayView> = unsafe { msg_send![super(view), initWithFrame: bounds] };
        let data = view.ivars();
        data.effect.set(effect);
        data.center.set(NSPoint::new(400.0, 225.0));
        data.radius.set(100.0);
        data.shade.set(0.6);
        data.zoom.set(2.0);
        if effect == Effect::Magnify {
            data.image.replace(Some(source.clone()));
        }
        if effect == Effect::Box {
            data.box_draw.set(BoxDraw::Rect(NSRect::new(
                NSPoint::new(300.0, 150.0),
                NSSize::new(200.0, 150.0),
            )));
        }
        let layer = bitmap()?;
        context(&layer)?;
        view.drawRect(bounds);
        let corner_alpha = layer
            .colorAtX_y(10, 10)
            .ok_or("Missing corner pixel")?
            .alphaComponent();
        let center_alpha = layer
            .colorAtX_y(400, 225)
            .ok_or("Missing center pixel")?
            .alphaComponent();
        match effect {
            Effect::Spotlight if (corner_alpha - 0.6).abs() > 0.02 || center_alpha > 0.01 => {
                return Err("Spotlight transparency check failed".into())
            }
            Effect::Laser if corner_alpha > 0.01 || center_alpha < 0.99 => {
                return Err("Laser transparency check failed".into())
            }
            Effect::Magnify if center_alpha < 0.99 => {
                return Err("Magnifier image check failed".into())
            }
            Effect::Box if (corner_alpha - 0.6).abs() > 0.02 || center_alpha > 0.01 => {
                return Err("Box transparency check failed".into())
            }
            _ => {}
        }
        let result = bitmap()?;
        context(&result)?;
        source.drawInRect_fromRect_operation_fraction(
            bounds,
            NSRect::ZERO,
            NSCompositingOperation::Copy,
            1.0,
        );
        image(&layer)?.drawInRect_fromRect_operation_fraction(
            bounds,
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
        );
        write(&result, &directory.join(format!("{name}.png")))?;
        println!("PASS {name}: corner alpha={corner_alpha:.3}, center alpha={center_alpha:.3}");
    }
    // Partial repaint: after a move, repainting only the computed dirty region
    // must leave exactly the pixels a full repaint would produce.
    type Change = fn(&ViewData);
    let scenarios: [(&str, Effect, Change); 5] = [
        ("spotlight move", Effect::Spotlight, |d| {
            d.center.set(NSPoint::new(530.0, 190.0))
        }),
        ("spotlight resize", Effect::Spotlight, |d| {
            d.radius.set(160.0)
        }),
        ("laser move", Effect::Laser, |d| {
            d.center.set(NSPoint::new(610.0, 300.0))
        }),
        ("magnifier move", Effect::Magnify, |d| {
            d.center.set(NSPoint::new(450.0, 260.0))
        }),
        ("box drag", Effect::Box, |d| {
            d.box_draw.set(BoxDraw::Rect(NSRect::new(
                NSPoint::new(260.0, 120.0),
                NSSize::new(330.0, 210.0),
            )))
        }),
    ];
    for (name, effect, change) in scenarios {
        let view = OverlayView::alloc(mtm).set_ivars(ViewData::default());
        let view: Retained<OverlayView> = unsafe { msg_send![super(view), initWithFrame: bounds] };
        let data = view.ivars();
        data.effect.set(effect);
        data.center.set(NSPoint::new(400.0, 225.0));
        data.radius.set(100.0);
        data.shade.set(0.6);
        data.zoom.set(2.0);
        if effect == Effect::Magnify {
            data.image.replace(Some(source.clone()));
        }
        if effect == Effect::Box {
            data.box_draw.set(BoxDraw::Rect(NSRect::new(
                NSPoint::new(300.0, 150.0),
                NSSize::new(200.0, 150.0),
            )));
        }
        let partial = bitmap()?;
        context(&partial)?;
        view.drawRect(bounds);
        let before = data.look();
        change(data);
        let Some(Some(dirty)) = repaint_region(before, data.look(), false) else {
            return Err(format!("{name}: expected a partial repaint").into());
        };
        NSGraphicsContext::saveGraphicsState_class();
        NSRectClip(dirty);
        view.drawRect(dirty);
        NSGraphicsContext::restoreGraphicsState_class();
        let full = bitmap()?;
        context(&full)?;
        view.drawRect(bounds);
        let bytes = |rep: &NSBitmapImageRep| {
            let len = (rep.bytesPerRow() * rep.pixelsHigh()) as usize;
            // SAFETY: the rep owns a contiguous buffer of bytesPerRow * height.
            unsafe { std::slice::from_raw_parts(rep.bitmapData(), len) }.to_vec()
        };
        let (a, b) = (bytes(&partial), bytes(&full));
        let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        let area = dirty.size.width * dirty.size.height / (800.0 * 450.0) * 100.0;
        if differing != 0 {
            return Err(format!("{name}: partial repaint differs in {differing} bytes").into());
        }
        println!("PASS partial repaint, {name}: identical, repainted {area:.1}% of the view");
    }
    // Menu bar glyph at 8x: solid pool and lamp, translucent cone. The system
    // tints the template image; its alpha is what this check verifies.
    let glyph_scale = 8.0;
    let layer = bitmap()?;
    context(&layer)?;
    status_glyph().drawInRect(NSRect::new(
        NSPoint::ZERO,
        NSSize::new(18.0 * glyph_scale, 18.0 * glyph_scale),
    ));
    // colorAtX_y counts rows from the top of the 450-pixel bitmap.
    let alpha_at = |x: f64, y: f64| {
        layer
            .colorAtX_y(
                (x * glyph_scale) as isize,
                (450.0 - y * glyph_scale) as isize,
            )
            .map(|c| c.alphaComponent())
            .unwrap_or(0.0)
    };
    let (pool, cone, lamp, outside) = (
        alpha_at(9.0, 3.9),
        alpha_at(9.0, 9.0),
        alpha_at(9.0, 14.9),
        alpha_at(1.0, 12.0),
    );
    if pool < 0.99 || lamp < 0.99 || !(0.3..0.6).contains(&cone) || outside > 0.01 {
        return Err(format!(
            "Status glyph alpha check failed: pool={pool:.2} cone={cone:.2} lamp={lamp:.2} outside={outside:.2}"
        )
        .into());
    }
    write(&layer, &directory.join("status-glyph.png"))?;
    println!("PASS status-glyph: pool alpha={pool:.3}, cone alpha={cone:.3}, lamp alpha={lamp:.3}");
    magnify_tile(mtm, &background, bounds)?;
    countdown_window(directory, mtm)?;
    NSGraphicsContext::setCurrentContext(None);
    Ok(())
}

/// Offscreen magnifier: the pointer-local tile must render exactly the pixels
/// the whole-screen frame rendered, and a 4K frame must cost less to rasterize.
fn magnify_tile(
    mtm: MainThreadMarker,
    background: &NSBitmapImageRep,
    bounds: NSRect,
) -> Result<(), Box<dyn std::error::Error>> {
    fn view_bitmap(
        mtm: MainThreadMarker,
        center: NSPoint,
        image: &Retained<NSImage>,
        image_rect: Option<NSRect>,
    ) -> Result<Retained<NSBitmapImageRep>, Box<dyn std::error::Error>> {
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(800.0, 450.0));
        let view = OverlayView::alloc(mtm).set_ivars(ViewData::default());
        let view: Retained<OverlayView> = unsafe { msg_send![super(view), initWithFrame: bounds] };
        let data = view.ivars();
        data.effect.set(Effect::Magnify);
        data.center.set(center);
        data.radius.set(60.0);
        data.shade.set(0.6);
        data.zoom.set(2.0);
        data.image.replace(Some(image.clone()));
        data.image_rect.set(image_rect);
        let layer = bitmap()?;
        context(&layer)?;
        view.drawRect(bounds);
        Ok(layer)
    }
    // SAFETY: one CIContext rasterizes a bitmap-backed CIImage on this thread.
    let ci_context = unsafe { CIContext::contextWithOptions(None) };
    let plain = background.CGImage().ok_or("Missing CGImage")?;
    let ci_source = unsafe { CIImage::imageWithCGImage(&plain) };
    let full = unsafe { ci_context.createCGImage_fromRect(&ci_source, bounds) }
        .ok_or("Full-frame rasterization failed")?;
    let full = NSImage::initWithCGImage_size(NSImage::alloc(), &full, bounds.size);
    for pointer in [NSPoint::new(400.0, 225.0), NSPoint::new(90.0, 70.0)] {
        let (pixel_rect, point_rect) = tile_bounds(bounds, pointer, 60.0, 2.0, 1.0)
            .ok_or("tile_bounds returned no tile for the QA view")?;
        let tile = unsafe { ci_context.createCGImage_fromRect(&ci_source, pixel_rect) }
            .ok_or("Tile rasterization failed")?;
        let tile = NSImage::initWithCGImage_size(NSImage::alloc(), &tile, point_rect.size);
        let whole = view_bitmap(mtm, pointer, &full, None)?;
        let crop = view_bitmap(mtm, pointer, &tile, Some(point_rect))?;
        let differing = bytes(&whole)
            .iter()
            .zip(&bytes(&crop))
            .filter(|(wide, narrow)| wide != narrow)
            .count();
        if differing != 0 {
            return Err(format!(
                "magnify tile crop at ({:.0}, {:.0}): {differing} bytes differ",
                pointer.x, pointer.y
            )
            .into());
        }
    }
    println!("PASS magnify tile crop: identical (radius=60, zoom=2, centre and lower left)");
    let extent_4k = NSRect::new(NSPoint::ZERO, NSSize::new(3840.0, 2160.0));
    let (tile_4k, _) = tile_bounds(extent_4k, NSPoint::new(960.0, 540.0), 150.0, 2.0, 2.0)
        .ok_or("4K tile bounds returned no tile")?;
    let share = tile_4k.size.width * tile_4k.size.height
        / (extent_4k.size.width * extent_4k.size.height)
        * 100.0;
    if share >= 10.0 {
        return Err(format!("4K magnifier tile is unexpectedly large: {share:.1}%").into());
    }
    println!(
        "PASS magnify 4K tile footprint: {:.0}x{:.0} of 3840x2160 ({share:.1}% of full screen)",
        tile_4k.size.width, tile_4k.size.height
    );
    Ok(())
}

/// Countdown panels without a device: window properties, the setup/running layout
/// switch, the last-ten-seconds colour, hiding on finish, and an offscreen render
/// of the real content view.
fn countdown_window(
    directory: &Path,
    mtm: MainThreadMarker,
) -> Result<(), Box<dyn std::error::Error>> {
    let screen = NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0));
    let countdown = CountdownWindow::new_for_frame(mtm, screen);
    let panel = countdown.qa_panel();
    // N6, N1 and N5: click-through, never key, above the black screen.
    if !panel.ignoresMouseEvents() || panel.canBecomeKeyWindow() || panel.canBecomeMainWindow() {
        return Err("Countdown window accepts clicks or can become key/main".into());
    }
    let level = panel.level();
    if level <= NSScreenSaverWindowLevel {
        return Err(format!("Countdown level {level} is not above the black screen").into());
    }
    countdown.render(Some(CountdownLook::Setup {
        minutes: 5,
        adjust: AdjustHint::Unavailable,
    }));
    let setup_frame = panel.frame();
    let expected_setup = NSRect::new(NSPoint::new(340.0, 330.0), NSSize::new(760.0, 240.0));
    if setup_frame != expected_setup {
        return Err(format!("Countdown setup panel is not centred: {setup_frame:?}").into());
    }
    if !panel.isVisible() {
        return Err("Countdown setup panel is not visible".into());
    }
    if countdown.qa_time_text() != "5 分钟" {
        return Err(format!("Countdown setup time is {:?}", countdown.qa_time_text()).into());
    }
    let hint = countdown.qa_hint_text();
    if hint != "无法用翻页键调整，3 秒后按 5 分钟开始" {
        return Err(format!("Countdown setup hint is {hint:?}").into());
    }
    // The panel is dark and its content view only draws the two labels, so any
    // opaque pixel proves the offscreen render produced text.
    let content = panel
        .contentView()
        .ok_or("Countdown panel has no content view")?;
    let content_rect = NSRect::new(NSPoint::ZERO, setup_frame.size);
    let layer = content
        .bitmapImageRepForCachingDisplayInRect(content_rect)
        .ok_or("Cannot allocate the countdown QA bitmap")?;
    content.cacheDisplayInRect_toBitmapImageRep(content_rect, &layer);
    write(&layer, &directory.join("countdown-setup.png"))?;
    let (mut ink, mut white) = (0usize, 0usize);
    for y in 0..layer.pixelsHigh() {
        for x in 0..layer.pixelsWide() {
            let Some(color) = layer.colorAtX_y(x, y) else {
                continue;
            };
            if color.alphaComponent() > 0.5 {
                ink += 1;
                if color.redComponent() > 0.9
                    && color.greenComponent() > 0.9
                    && color.blueComponent() > 0.9
                {
                    white += 1;
                }
            }
        }
    }
    if ink == 0 || white == 0 {
        return Err(format!("countdown-setup.png has no text: ink={ink} white={white}").into());
    }
    // N2: the setup panel becomes a small top-right countdown.
    countdown.render(Some(CountdownLook::Running { seconds: 11 }));
    let running_frame = panel.frame();
    let expected_running = NSRect::new(NSPoint::new(1088.0, 764.0), NSSize::new(320.0, 104.0));
    if running_frame != expected_running {
        return Err(format!("Countdown running panel is not top right: {running_frame:?}").into());
    }
    if countdown.qa_time_text() != "00:11" || countdown.qa_is_red() {
        return Err("Countdown at 11 s is not white 00:11".into());
    }
    // N3: red only in the last ten seconds.
    countdown.render(Some(CountdownLook::Running { seconds: 10 }));
    if countdown.qa_time_text() != "00:10" || !countdown.qa_is_red() {
        return Err("Countdown at 10 s is not red 00:10".into());
    }
    countdown.render(Some(CountdownLook::Up));
    if countdown.qa_time_text() != "时间到" {
        return Err(format!("Countdown finish time is {:?}", countdown.qa_time_text()).into());
    }
    // N4: going idle hides the panel.
    countdown.render(None);
    if panel.isVisible() {
        return Err("Countdown panel stays visible when idle".into());
    }
    countdown.retire();
    println!(
        "PASS countdown window: level={level} setup={:.0}x{:.0} running={:.0}x{:.0} text=5 分钟/00:11/00:10/时间到 ink={ink} white={white}",
        setup_frame.size.width,
        setup_frame.size.height,
        running_frame.size.width,
        running_frame.size.height
    );
    Ok(())
}
