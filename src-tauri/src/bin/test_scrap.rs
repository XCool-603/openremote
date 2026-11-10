fn main() {
    let displays = scrap::Display::all();
    println!("Displays result: {:?}", displays.as_ref().map(|d| d.len()));
    let display = scrap::Display::primary().expect("No primary display");
    println!("Primary display: {}x{}", display.width(), display.height());
    match scrap::Capturer::new(display) {
        Ok(mut c) => {
            println!("Scrap Capturer created successfully!");
            for i in 0..15 {
                match c.frame() {
                    Ok(f) => {
                        println!("🎉 Captured real frame {}: {} bytes", i, f.len());
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(e) => {
                        println!("Frame error: {}", e);
                        break;
                    }
                }
            }
        }
        Err(e) => {
            println!("Scrap Capturer failed: {}", e);
        }
    }
}
