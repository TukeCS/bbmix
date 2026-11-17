//! bbmix: a mixer for the RME Babyface Pro (FS) on Linux.

mod device;
mod hw;
mod model;
mod png;
mod ui;

const USAGE: &str = "usage: bbmix [--demo] [--screenshot OUT.png]

  --demo              don't touch the hardware (try the UI without the interface)
  --screenshot FILE   render one frame to a PNG and quit

Your mix is saved to ~/.config/bbmix/state.json (or $BBMIX_STATE) and put back on the
interface every time it connects.";

fn main() -> eframe::Result {
    let mut opts = ui::Opts { demo: false, screenshot: None };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--demo" => opts.demo = true,
            "--screenshot" => match args.next() {
                Some(p) => opts.screenshot = Some(p.into()),
                None => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => {
                eprintln!("unknown option {other:?}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("bbmix")
            .with_app_id("bbmix")
            .with_inner_size([1240.0, 1000.0])
            .with_min_inner_size([900.0, 600.0]),
        persist_window: opts.screenshot.is_none(),
        ..Default::default()
    };
    eframe::run_native("bbmix", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc, opts)))))
}
