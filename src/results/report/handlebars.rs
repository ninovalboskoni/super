//! Handlebars report generation module.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::Path,
};

use colored::Colorize;
use failure::{Error, ResultExt};
use handlebars::Handlebars;
use serde_json::{value::Value, Map};

use crate::{
    config::Config,
    copy_folder, error,
    results::{
        handlebars_helpers::{
            all_code, all_lines, generate_menu, html_code, line_numbers, report_index,
        },
        report::Generator,
        utils::html_escape,
        Results,
    },
};

/// Handlebars report generator.
pub struct Report {
    /// Handlebars template structure.
    handler: Handlebars,
    /// Package name.
    package: String,
}

impl Report {
    /// Creates a new handlebars report generator.
    pub fn from_path<P: AsRef<Path>, S: Into<String>>(
        template_path: P,
        package: S,
    ) -> Result<Self, Error> {
        let handlebars_handler =
            Self::load_templates(template_path).context("Could not load handlebars templates")?;

        Ok(Self {
            handler: handlebars_handler,
            package: package.into(),
        })
    }

    /// Loads templates from the given path.
    fn load_templates<P: AsRef<Path>>(template_path: P) -> Result<Handlebars, Error> {
        let mut handlebars = Handlebars::new();
        handlebars.register_escape_fn(|s| html_escape(s).into_owned());
        let _ = handlebars.register_helper("line_numbers", Box::new(line_numbers));
        let _ = handlebars.register_helper("html_code", Box::new(html_code));
        let _ = handlebars.register_helper("report_index", Box::new(report_index));
        let _ = handlebars.register_helper("all_code", Box::new(all_code));
        let _ = handlebars.register_helper("all_lines", Box::new(all_lines));
        let _ = handlebars.register_helper("generate_menu", Box::new(generate_menu));
        for dir_entry in fs::read_dir(template_path)? {
            let dir_entry = dir_entry?;
            if let Some(ext) = dir_entry.path().extension() {
                if ext == "hbs" {
                    let path = dir_entry.path();
                    let template_file = path
                        .file_stem()
                        .ok_or_else(|| error::Kind::TemplateName {
                            message: "template files must have a file name".to_owned(),
                        })
                        .and_then(|stem| {
                            stem.to_str().ok_or_else(|| error::Kind::TemplateName {
                                message: "template names must be unicode".to_string(),
                            })
                        })?;

                    handlebars
                        .register_template_file(template_file, dir_entry.path())
                        .context("error registering template file")?;
                }
            }
        }

        if handlebars.get_template("report").is_none()
            || handlebars.get_template("src").is_none()
            || handlebars.get_template("code").is_none()
        {
            let message = format!(
                "templates must include {}, {} and {} templates",
                "report".italic(),
                "src".italic(),
                "code".italic()
            );

            Err(error::Kind::TemplateName { message }.into())
        } else {
            Ok(handlebars)
        }
    }

    /// Generates the HTML files for the code.
    fn generate_code_html_files(&self, config: &Config, results: &Results) -> Result<(), Error> {
        let menu = Value::Array(self.generate_code_html_folder("", config, results)?);

        let mut f = File::create(
            config
                .results_folder()
                .join(&results.app_package())
                .join("src")
                .join("index.html"),
        )?;

        let mut data = BTreeMap::new();
        let _ = data.insert("menu", menu);
        f.write_all(self.handler.render("src", &data)?.as_bytes())?;

        Ok(())
    }

    /// Generates a folder with HTML files with the source code of the application.
    fn generate_code_html_folder<P: AsRef<Path>>(
        &self,
        path: P,
        config: &Config,
        results: &Results,
    ) -> Result<Vec<Value>, Error> {
        if path.as_ref() == Path::new("classes/android")
            || path.as_ref() == Path::new("classes/com/google/android/gms")
            || path.as_ref() == Path::new("smali")
        {
            return Ok(Vec::new());
        }
        let dir_iter = fs::read_dir(config.dist_folder().join(&self.package).join(path.as_ref()))?;

        fs::create_dir_all(
            config
                .results_folder()
                .join(&results.app_package())
                .join("src")
                .join(path.as_ref()),
        )?;

        let mut menu = Vec::new();
        for entry in dir_iter {
            let entry = entry?;
            let path = entry.path();

            let prefix = config.dist_folder().join(&self.package);
            let stripped = path
                .strip_prefix(&prefix)
                .expect("could not remove path prefix");

            if path.is_dir() {
                if stripped != Path::new("original") {
                    let inner_menu = self.generate_code_html_folder(stripped, config, results)?;
                    if inner_menu.is_empty() {
                        let path = config
                            .results_folder()
                            .join(&results.app_package())
                            .join("src")
                            .join(stripped);
                        if path.exists() {
                            fs::remove_dir_all(path)?;
                        }
                    } else {
                        let mut object = Map::with_capacity(2);
                        let name = path.file_name().unwrap().to_string_lossy().into_owned();

                        let _ = object.insert("name".to_owned(), Value::String(name));
                        let _ = object.insert("menu".to_owned(), Value::Array(inner_menu));
                        menu.push(Value::Object(object));
                    }
                }
            } else {
                match path.extension() {
                    Some(e) if e == "xml" || e == "java" => {
                        self.generate_code_html_for(&stripped, config, results, &self.package)?;
                        let name = path.file_name().unwrap().to_string_lossy().into_owned();
                        let mut data = Map::with_capacity(3);
                        let _ = data.insert("name".to_owned(), Value::String(name));
                        let _ = data.insert(
                            "path".to_owned(),
                            Value::String(format!("{}", stripped.display())),
                        );
                        let _ = data.insert(
                            "type".to_owned(),
                            Value::String(e.to_string_lossy().into_owned()),
                        );
                        menu.push(Value::Object(data));
                    }
                    _ => {}
                }
            }
        }

        Ok(menu)
    }

    /// Generates an HTML file with source code for the given path.
    fn generate_code_html_for<P: AsRef<Path>, S: AsRef<str>>(
        &self,
        path: P,
        config: &Config,
        results: &Results,
        cli_package_name: S,
    ) -> Result<(), Error> {
        let code = fs::read_to_string(
            config
                .dist_folder()
                .join(cli_package_name.as_ref())
                .join(path.as_ref()),
        )?;
        let mut f_out = File::create(format!(
            "{}.html",
            config
                .results_folder()
                .join(&results.app_package())
                .join("src")
                .join(path.as_ref())
                .display()
        ))?;

        let mut back_path = String::new();
        for _ in path.as_ref().components() {
            back_path.push_str("../");
        }

        let mut data = BTreeMap::new();
        let _ = data.insert(
            String::from("path"),
            Value::String(format!("{}", path.as_ref().display())),
        );
        let _ = data.insert(String::from("code"), Value::String(code));
        let _ = data.insert(String::from("back_path"), Value::String(back_path));

        f_out.write_all(self.handler.render("code", &data)?.as_bytes())?;

        Ok(())
    }
}

impl Generator for Report {
    #[allow(clippy::print_stdout)]
    fn generate(&mut self, config: &Config, results: &Results) -> Result<(), Error> {
        if config.is_verbose() {
            println!("Starting HTML report generation. First we create the file.")
        }
        let mut f = File::create(
            config
                .results_folder()
                .join(&results.app_package)
                .join("index.html"),
        )?;
        if config.is_verbose() {
            println!("The report file has been created. Now it's time to fill it.")
        }

        f.write_all(self.handler.render("report", results)?.as_bytes())?;

        for entry in fs::read_dir(config.template_path())? {
            let entry = entry?;
            let entry_path = entry.path();
            if entry.file_type()?.is_dir() {
                copy_folder(
                    &entry_path,
                    &config
                        .results_folder()
                        .join(&results.app_package())
                        .join(entry_path.file_name().unwrap()),
                )?;
            } else {
                match entry_path.as_path().extension() {
                    Some(e) if e == "hbs" => {}
                    None => {}
                    _ => {
                        let _ = fs::copy(
                            &entry_path,
                            &config.results_folder().join(&results.app_package()),
                        )?;
                    }
                }
            }
        }

        self.generate_code_html_files(config, results)?;

        Ok(())
    }
}

/// Handlebars templates testing module.
#[cfg(test)]
mod test {
    use super::Report;
    use crate::config::Config;

    /// Test the creation of a new report.
    #[test]
    fn it_new() {
        let _ = Report::from_path(&Config::default().template_path(), "test").unwrap();
    }

    /// Test the failure of the creation of an invalid new report.
    #[test]
    fn it_new_failure() {
        assert!(Report::from_path("random path", "test").is_err());
    }

    /// Tests handlebars template loading.
    #[test]
    fn it_load_templates() {
        let _ = Report::load_templates(&Config::default().template_path()).unwrap();
    }
}
