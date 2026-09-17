mod ui;

use gtk::prelude::*;

fn main() -> gtk::glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("io.github.pkkulhari.Notebook")
        .build();
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
        } else {
            ui::launch(app);
        }
    });
    app.run()
}
