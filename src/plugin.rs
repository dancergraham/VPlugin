/*
 * Copyright 2022 Aggelos Tselios.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0

 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
*/

#![allow(dead_code)]

extern crate libloading;
extern crate log;

use std::env::{self};
use std::ffi::OsStr;
use std::fs::{
        self,
        File
};
use serde::Deserialize;
use serde_derive::Deserialize;
use libloading::{
        Library,
        Symbol
};
use zip::ZipArchive;
use crate::VHook;
use crate::error::VPluginError;
use std::io::ErrorKind::{*, self};

/* Personally I believe it looks much better like this */
type LaterInitialized<T> = Option<T>;
macro_rules! initialize_later {
    () => {
        None
    };
}
macro_rules! init_now {
    ($a:expr) => {
        Some($a)
    };
}

/// This is purely for deserialization.
#[derive(Deserialize)]
struct Data {
        metadata: Metadata
}

#[derive(Deserialize)]
struct Metadata {
        description: Option<String>,
        version    : String,
        name       : String,
        objfile    : String
}
/// A struct that represents metadata about
/// a single plugin, like its version and name.
/// 
/// This struct should only be returned by `PluginMetadata::load()`.
/// Otherwise, undefined values will be returned, resulting in undefined
/// behavior.
#[derive(Debug)]
#[repr(C)]
pub struct PluginMetadata {
        pub description: Option<String>,
        pub version    : String,
        pub name       : String,
        pub filename   : String,
        pub objfile    : String
}

/// The plugin type. This is used to identify a single plugin
/// from VPlugin. New plugins should be loaded with `Plugin::load()`,
/// and not be reused explicitly.
#[derive(Debug)]
#[repr(C)]
pub struct Plugin {
        // Metadata about the plugin, will be None if the plugin
        // has not loaded its metadata yet.
        pub metadata       : LaterInitialized<PluginMetadata>,
        pub(crate) filename: String,
        pub(crate) is_valid: bool,
        pub(crate) started : bool,
        pub(crate) raw     : LaterInitialized<Library>,
        pub(crate) archive : ZipArchive<File>,

}

impl PluginMetadata {
        /// Reads a metadata.toml file or returns an error. This is useful
        /// for libraries that wish to make use of VPlugin's internals.
        pub fn read_from_str<T: for<'a> Deserialize<'a>>(string: &str) -> Result<T, VPluginError> {
                let data: T = match toml::from_str(string) {
                        Ok (t) => t,
                        Err(e) => {
                                log::error!("Couldn't read metadata file: {}", e.to_string());
                                return Err(VPluginError::ParametersError)
                        }
                };

                Ok(data)

        }
        
        fn load(plugin: &Plugin) -> Result<Self, VPluginError> {
                let mut plugin_metadata = Self {
                     description: None,
                     version    : String::new(),
                     name       : String::new(),
                     filename   : plugin.filename.clone(),
                     objfile    : String::new(),
                };

                let f = match File::open("metadata.toml") {
                        Ok(val) => val,
                        Err(e) => {
                                match e.kind() {
                                        PermissionDenied => return Err(VPluginError::PermissionDenied),
                                        Unsupported      => return Err(VPluginError::InternalError { err: "Unsupported file".into() }),
                                        NotFound         => return Err(VPluginError::NoSuchFile),
                                        Interrupted      => return Err(VPluginError::InvalidPlugin),
                                        UnexpectedEof    => return Err(VPluginError::InvalidPlugin),
                                        OutOfMemory      => return Err(VPluginError::InternalError { err: "Host is out of memory".into() }),
                                        Other            => return Err(VPluginError::InternalError { err: "Unknown error.".into() }),
                                        _ => panic!()
                                }
                        }
                };

                let contents = match std::io::read_to_string(f) {
                        Ok(contents) => contents,
                        Err(e)        => {
                                log::error!("Error reading metadata string: {}.", e.to_string());
                                return Err(VPluginError::ParametersError);
                        }
                };
                let buffer = String::from(contents.as_str());

                let data_raw: Data = match toml::from_str(&buffer) {
                        Ok(ok) => ok,
                        Err(_) => {
                                return Err(VPluginError::ParametersError)
                        }
                };

                if data_raw.metadata.name.is_empty()
                || data_raw.metadata.name.contains(' ') {
                        /*
                         * Here we panic as without a name, it's impossible to identify the plugin
                         * for future errors.
                         */
                        panic!(
                                "
                                Attempted to use a plugin that has an empty name in its metadata or contains an
                                invalid character in the field.
                                "
                        )
                }

                if data_raw.metadata.version.is_empty()
                || data_raw.metadata.version.contains(' ') {
                        log::error!(
                                "
                                Detected either empty or invalid version string in metadata.toml (Plugin
                                '{}'
                                ", data_raw.metadata.name
                        );
                }

                plugin_metadata.filename = "metadata.toml".to_owned();
                plugin_metadata.version  = data_raw.metadata.version;
                plugin_metadata.name     = data_raw.metadata.name;
                plugin_metadata.objfile  = data_raw.metadata.objfile;

                Ok(plugin_metadata)
        }
}

impl Plugin {
        fn load_archive<S: Copy + Into<String> + AsRef<OsStr>>(filename: S) -> Result<Self, VPluginError> {
                log::trace!("Loading plugin: {}.", &filename.into());
                let tmp = filename.into();
                let fname = std::path::Path::new(&tmp);
                let file = match fs::File::open(fname) {
                        Ok(val) => val,
                        Err(e) => {
                                log::error!(
                                        "Couldn't load {}: {} (error {})",
                                        filename.into(),
                                        e.to_string(),
                                        e.raw_os_error().unwrap_or(0)
                                );
                                match e.kind() {
                                        PermissionDenied => return Err(VPluginError::PermissionDenied),
                                        Unsupported      => return Err(VPluginError::InternalError { err: "Unsupported file".into() }),
                                        NotFound         => return Err(VPluginError::NoSuchFile),
                                        Interrupted      => return Err(VPluginError::InvalidPlugin),
                                        UnexpectedEof    => return Err(VPluginError::InvalidPlugin),
                                        OutOfMemory      => return Err(VPluginError::InternalError { err: "Host is out of memory".into() }),
                                        Other            => return Err(VPluginError::InternalError { err: "Unknown error.".into() }),
                                        _ => panic!()
                                }
                        }
                };
                
                match std::fs::create_dir(env::temp_dir().join("vplugin")) {
                        Err(e) => match e.kind() {
                                ErrorKind::AlreadyExists => (),
                                _ => log::info!("Couldn't create VPlugin directory: {}", e.to_string()),
                        }
                        Ok(_) => env::set_current_dir(env::temp_dir().join("vplugin")).unwrap()
                }

                /* Uncompressing the archive. */
                log::trace!("Uncompressing plugin {}", filename.into());
                let mut archive = zip::ZipArchive::new(file).unwrap();
                for i in 0..archive.len() {
                        let mut file = archive.by_index(i).unwrap();
                        let outpath = match file.enclosed_name() {
                            Some(path) => path.to_owned(),
                            None => continue,
                        };

                        if (*file.name()).ends_with('/') {
                                fs::create_dir_all(&outpath).unwrap();
                        } else {
                                if let Some(p) = outpath.parent() {
                                        if !p.exists() {
                                            fs::create_dir_all(p).unwrap();
                                        }
                                }
                                
                                let mut outfile = fs::File::create(&outpath).unwrap();
                                std::io::copy(&mut file, &mut outfile).unwrap();
                        }
                }

                let plugin = Self {
                        metadata: initialize_later!(),
                        raw     : initialize_later!(),
                        filename: filename.into(),
                        is_valid: false,
                        started : false,
                        archive,
                };

                Ok(plugin)
        }
        /// Loads a plugin into memory and returns it.
        /// After 0.2.0, metadata is also loaded in this call so avoid calling it
        /// again (For your convenience, it has been marked as deprecated).
        pub fn load<S: Copy + Into<String> + AsRef<OsStr>>(filename: S) -> Result<Plugin, VPluginError> {
                let mut plugin = match Self::load_archive(filename) {
                        Err(e) => {
                                log::error!("Couldn't load archive, stopping here.");
                                return Err(e);
                        }
                        Ok (p) => p
                };
                
                /* Until I rewrite the function a little, we shouldn't care about the warning. */
                #[allow(deprecated)]
                match plugin.load_metadata() {
                        Err(e) => {
                                log::error!("Couldn't load metadata, stopping here.");
                                return Err(e);
                        }
                        Ok(_) => {
                                fs::create_dir_all(
                                        env::temp_dir()
                                        .join("vplugin")
                                        .join(&plugin.metadata.as_ref().unwrap().name)
                                ).expect("Cannot create plugin directory!");
                        }
                }
                Ok(plugin)
        }

        /// Returns a VHook (Generic function pointer) that can be used to exchange data between
        /// your application and the plugin.
        pub(super) fn load_vhook(&self, fn_name: &str) -> Result<VHook, VPluginError> {
                if !self.started || !self.is_valid || self.raw.is_none() {
                        log::error!("Attempted to load plugin function that isn't started or isn't valid");
                        return Err(VPluginError::InvalidPlugin);
                }
                let hook: Symbol<VHook>;
                unsafe {
                        hook = match self.raw
                                .as_ref()
                                .unwrap_unchecked()
                                .get(format!("{}\0", fn_name).as_bytes())
                        {
                            Ok (v) => v,
                            Err(_) => return Err(VPluginError::MissingSymbol),
                        };
                }
                Ok(*hook)
        }

        pub(crate) fn get_hook(&self, fn_name: &str) -> Result<VHook, VPluginError> {
                Self::load_vhook(self, fn_name)
        }

        /// Implemented as public in [PluginManager](crate::plugin_manager::PluginManager).
        pub(crate) fn get_custom_hook<P, T>(
                &self,
                fn_name: &str
        ) -> Result<unsafe extern fn(P) -> T, VPluginError> {
                if !self.started || !self.is_valid || self.raw.is_none() {
                        log::error!("Cannot load custom hook from non-started or invalid plugin.");
                        return Err(VPluginError::InvalidPlugin);
                }
                let hook: Symbol<unsafe extern fn(P) -> T>;
                unsafe {
                        hook = match self.raw
                                .as_ref()
                                .unwrap_unchecked()
                                .get(format!("{}\0", fn_name).as_bytes())
                        {
                            Ok (v) => v,
                            Err(_) => return Err(VPluginError::MissingSymbol),
                        };
                }
                Ok(*hook)
        }

        /// A function to load the plugin's metadata into
        /// the plugin. In order to access the plugin's metadata,
        /// use the [get_metadata](crate::plugin::Plugin::get_metadata) function.
        /// See also: [PluginMetadata](crate::plugin::PluginMetadata)
        #[deprecated = "The plugin's metadata will be automatically loaded along with the plugin itself."]
        pub fn load_metadata(&mut self) -> Result<(), VPluginError> {
                match PluginMetadata::load(self) {
                        Ok (v) => {
                                let plugin_dir_name = env::temp_dir()
                                        .join("vplugin")
                                        .join(&v.name);

                                fs::create_dir_all(&plugin_dir_name).unwrap();
                                fs::copy(&v.objfile, plugin_dir_name.join(&v.objfile)).unwrap();

                                self.raw       = unsafe {
                                        init_now!(Library::new(plugin_dir_name.join(&v.objfile)).unwrap())
                                };
                                self.is_valid = true;
                                self.metadata = init_now!(v);

                                Ok(())
                        },
                        Err(e) => {
                                log::error!("Couldn't load metadata ({}): {}", self.filename, e.to_string());
                                Err(e)
                        }
                }
        }

        /// Returns a reference to the plugin metadata, if loaded.
        /// Otherwise, `None` is returned.
        pub fn get_metadata(&self) -> &Option<PluginMetadata> {
                &self.metadata
        }

        /// Unloads the plugin, if loaded and started,
        /// calling its destructor in the process and
        /// freeing up resources.
        /// 
        /// ## `Err` returned:
        /// If an `Err` value was returned, this means that
        /// the plugin was either not loaded, invalid, doesn't
        /// have a destructor function. In that case, you can try
        /// using [`Plugin::force_terminate`](crate::plugin::Plugin::force_terminate)
        /// to force the plugin to be removed, risking safety and undefined behavior.
        pub fn terminate(&mut self) -> Result<(), VPluginError> {
                if self.raw.is_none() {
                        return Err(VPluginError::InvalidPlugin);
                }

                if !self.started {
                        log::error!("Cannot terminate a plugin that wasn't started in the first place.");
                        return Err(VPluginError::InvalidPlugin);
                }

                let destructor: Symbol<unsafe extern "C" fn() -> ()>;
                unsafe {
                        destructor = match self.raw
                                .as_ref()
                                .unwrap_unchecked()
                                .get(b"vplugin_exit\0")
                        {
                            Ok (v) => v,
                            Err(_) => {
                                log::warn!(
                                        target: "Destructor",
                                        "Plugin {} does not have a destructor. Force terminate if needed.",
                                        self.get_metadata().as_ref().unwrap().name
                                );
                                return Err(VPluginError::InvalidPlugin)
                            },
                        };

                        destructor();
                }

                self.started  = false;
                if cfg!(feature = "non_reusable_plugins") {
                        self.is_valid = false;
                        self.raw      = None;
                        self.filename = String::new();
                        self.metadata = None;
                }
                Ok(())
        }

        /// Returns whether the function specified is available on the plugin.
        pub fn is_function_available(&self, name: &str) -> bool {
                if self.raw.is_none() {
                        log::warn!("Avoid using misinitialized plugins as properly loaded ones (Missing shared object file).");
                        return false;
                }
                unsafe {
                        self.raw.as_ref().unwrap().get::<unsafe extern "C" fn()>(name.as_bytes()).is_ok()
                }
        }

        /// Returns whether the plugin metadata is available
        /// and loaded. You can use this to avoid unwrap()'ing
        /// on invalid values.
        #[inline(always)]
        pub fn is_metadata_loaded(&self) -> bool {
                self.metadata.is_some()
        }
}

impl Drop for Plugin {
        fn drop(&mut self) {
                let plugin_dir_name = env::temp_dir()
                        .join("vplugin")
                        .join(&self.metadata.as_ref().unwrap().name);

                match std::fs::remove_dir_all(&plugin_dir_name) {
                        Err(e) => {
                                log::warn!(
                                        "Couldn't remove directory '{}' corresponding to plugin '{}': {}",
                                        plugin_dir_name.display(),
                                        self.metadata.as_ref().unwrap().name,
                                        e.to_string()
                                )
                        },
                        Ok(_) => ()
                }
        }
}
