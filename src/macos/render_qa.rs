//! Deterministic offscreen checks of the same NSView drawing used by live panels.
//! Synthetic imagery only: no desktop capture or screen permission is needed.
use super::*;
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
        let layer = bitmap()?;
        context(&layer)?;
        view.draw(sel!(drawRect:), bounds);
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
    NSGraphicsContext::setCurrentContext(None);
    Ok(())
}
