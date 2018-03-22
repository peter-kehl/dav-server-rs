//! Utility module to handle the "path" part of an URL as a filesytem path.
//!
use std;
use std::error::Error;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path,PathBuf};

use hyper;
use mime_guess;
use percent_encoding as pct;

use super::DavError;

/// Path information relative to a prefix.
#[derive(Clone)]
pub struct WebPath {
    pub(crate) path:    Vec<u8>,
    pub(crate) prefix:  Vec<u8>,
}

define_encode_set! {
    pub ENCODE_SET = [pct::DEFAULT_ENCODE_SET] | {'&', '%'}
}

impl std::fmt::Display for WebPath {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", &self.as_url_string_with_prefix_debug())
    }
}

impl std::fmt::Debug for WebPath {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", &self.as_url_string_with_prefix_debug())
    }
}

#[derive(Debug)]
pub enum ParseError {
    // cannot parse
    InvalidPath,
    // outside of prefix
    IllegalPath,
    // too many dotdots
    ForbiddenPath,
}

impl Error for ParseError {
    fn description(&self) -> &str {
        "WebPath parse error"
    }
    fn cause(&self) -> Option<&Error> { None }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<ParseError> for DavError {
    fn from(e: ParseError) -> Self {
        match e {
            ParseError::InvalidPath => DavError::InvalidPath,
            ParseError::IllegalPath => DavError::IllegalPath,
            ParseError::ForbiddenPath => DavError::ForbiddenPath,
        }
    }
}

// a decoded segment can contain any value except '/' or '\0'
fn valid_segment(src: &[u8]) -> Result<(), ParseError> {
    let mut p = pct::percent_decode(src);
    if p.any(|x| x == 0 || x == b'/') {
        return Err(ParseError::InvalidPath);
    }
    Ok(())
}

// encode path segment with user-defined ENCODE_SET
fn encode_path(src: &[u8]) -> Vec<u8> {
    pct::percent_encode(src, ENCODE_SET).to_string().into_bytes()
}

// make path safe:
// - raw path before decoding can contain only printable ascii
// - make sure path is absolute
// - remove query part (everything after ?)
// - merge consecutive slashes
// - process . and ..
// - decode percent encoded bytes, fail on invalid encodings.
// - do not allow NUL or '/' in segments.
fn normalize_path(rp: &[u8]) -> Result<Vec<u8>, ParseError> {

    // must consist of printable ASCII
    if rp.iter().any(|&x| x < 32 || x > 126) {
        Err(ParseError::InvalidPath)?;
    }

    // delete query part (if any)
    let mut rawpath = rp;
    if let Some(pos) = rawpath.iter().position(|&x| x == b'?') {
        rawpath = &rawpath[..pos];
    }

    // must start with "/"
    if rawpath.is_empty() || rawpath[0] != b'/' {
        Err(ParseError::InvalidPath)?;
    }

    // split up in segments
    let isdir = match rawpath.last() {
        Some(x) if *x == b'/' => true,
        _ => false,
    };
    let segments = rawpath.split(|c| *c == b'/');
    let mut v : Vec<&[u8]>  = Vec::new();
    for segment in segments {
        match segment {
            b"." | b"" => {},
            b".." => {
                if v.len() < 2 {
                    return Err(ParseError::ForbiddenPath);
                }
                v.pop(); v.pop();
            },
            s => {
                if let Err(e) = valid_segment(s) {
                    Err(e)?;
                }
                v.push(b"/");
                v.push(s);
            }
        }
    }
    if isdir || v.is_empty() {
        v.push(b"/");
    }
    Ok(v.iter().flat_map(|s| pct::percent_decode(s)).collect())
}

/// Comparision ignores any trailing slash, so /foo == /foo/
impl PartialEq for WebPath {
    fn eq(&self, rhs: &WebPath) -> bool {
        let mut a = self.path.as_slice();
        if a.len() > 1 && a.ends_with(b"/") {
            a = &a[..a.len()-1];
        }
        let mut b = rhs.path.as_slice();
        if b.len() > 1 && b.ends_with(b"/") {
            b = &b[..b.len()-1];
        }
        self.prefix == rhs.prefix && a == b
    }
}

impl WebPath {
    /// from URL encoded strings: path and prefix.
    pub fn from_str(src: &str, prefix: &str) -> Result<WebPath, ParseError> {
        let b = src.as_bytes();
        let path = normalize_path(b)?;
        let mut prefix = prefix.as_bytes();
        if !path.starts_with(prefix) {
            return Err(ParseError::IllegalPath);
        }
        let pflen = prefix.len();
        if prefix.ends_with(b"/") {
            prefix = &prefix[..pflen-1];
        } else if path.len() != pflen &&
                  (path.len() < pflen || path[pflen] != b'/') {
            return Err(ParseError::IllegalPath);
        }
        Ok(WebPath{
            path:   path[prefix.len()..].to_vec(),
            prefix: prefix.to_vec(),
        })
    }

    /// from hyper req.uri
    pub fn from_uri(uri: &hyper::uri::RequestUri, prefix: &str) -> Result<Self, ParseError> {
        match uri {
            &hyper::uri::RequestUri::AbsolutePath(ref r) => {
                WebPath::from_str(r, prefix)
            },
            &hyper::uri::RequestUri::Star => {
                Ok(WebPath{
                    prefix: b"".to_vec(),
                    path: b"*".to_vec(),
                })
            },
            _ => {
                Err(ParseError::InvalidPath)
            }
        }
    }

    /// from hyper Url and (not-url-encoded) prefix string.
    pub fn from_url(url: &hyper::Url, prefix: &str) -> Result<Self, ParseError> {
        WebPath::from_str(url.path(), prefix)
    }

    // is this a "star" request (only used with OPTIONS)
    pub(crate) fn is_star(&self) -> bool {
        self.path == b"*"
    }

    // as URL encoded string.
    pub(crate) fn as_url_string(&self) -> String {
        let p = encode_path(&self.path);
        std::string::String::from_utf8(p).unwrap()
    }

    // as URL encoded string, with prefix.
    pub(crate) fn as_url_string_with_prefix(&self) -> String {
        let mut p = encode_path(&self.path);
        if self.prefix.len() > 0 {
            let mut u = encode_path(&self.prefix);
            u.extend_from_slice(&p);
            p = u;
        }
        std::string::String::from_utf8(p).unwrap()
    }

    // as URL encoded string, with prefix.
    pub(crate) fn as_url_string_with_prefix_debug(&self) -> String {
        let mut p = encode_path(&self.path);
        if self.prefix.len() > 0 {
            let mut u = encode_path(&self.prefix);
            u.extend_from_slice(b"[");
            u.extend_from_slice(&p);
            u.extend_from_slice(b"]");
            p = u;
        }
        std::string::String::from_utf8(p).unwrap()
    }

    // as utf8 string, with prefix.
    pub(crate) fn as_utf8_string_with_prefix(&self) -> String {
        let mut p = self.prefix.clone();
        p.extend_from_slice(&self.path);
        return String::from_utf8_lossy(&p).to_string();
    }

    /// as raw bytes, not encoded, no prefix.
    pub fn as_bytes(&self) -> &[u8] {
        self.path.as_slice()
    }

    /// as OS specific Path. never ends in "/".
    pub fn as_pathbuf(&self) -> PathBuf {
        let mut b = self.path.as_slice();
        if b.len() > 1 && b.ends_with(b"/") {
            b = &b[..b.len()-1];
        }
        let os_string = OsStr::from_bytes(b).to_owned();
        PathBuf::from(os_string)
    }

    /// prefix the WebPath with a Path and return a PathBuf
    pub fn as_pathbuf_with_prefix<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        let mut p = path.as_ref().to_path_buf();
        p.push(self.as_rel_pathbuf());
        p
    }

    /// as OS specific Path, relative (remove first slash)
    pub fn as_rel_pathbuf(&self) -> PathBuf {
        let mut path = if self.path.len() > 0 {
            &self.path[1..]
        } else {
            &self.path
        };
        if path.ends_with(b"/") {
            path = &path[..path.len()-1];
        }
        let os_string = OsStr::from_bytes(path).to_owned();
        PathBuf::from(os_string)
    }

    /// is this a collection i.e. does the original URL path end in "/".
    pub fn is_collection(&self) -> bool {
        let l = self.path.len();
        l > 0 && self.path[l-1] == b'/'
    }

    /// return the URL prefix.
    pub fn prefix(&self) -> &str {
        std::str::from_utf8(&self.prefix).unwrap()
    }

    // remove trailing slash
    #[allow(unused)]
    pub(crate) fn remove_slash(&mut self) {
        let mut l = self.path.len();
        while l > 1 && self.path[l-1] == b'/' {
            l -= 1;
        }
        self.path.truncate(l);
    }

    // add a slash
    pub(crate) fn add_slash(&mut self) {
        if !self.is_collection() {
            self.path.push(b'/');
        }
    }

    // add a slash
    pub(crate) fn add_slash_if(&mut self, b: bool) {
        if b && !self.is_collection() {
            self.path.push(b'/');
        }
    }

    // get parent.
    pub(crate) fn parent(&self) -> WebPath {
        let mut segs = self.path.split(|&c| c == b'/').filter(|e| e.len() > 0).collect::<Vec<&[u8]>>();
        segs.pop();
        if segs.len() > 0 {
            segs.push(b"");
        }
        segs.insert(0, b"");
        WebPath{
            prefix: self.prefix.clone(),
            path: segs.join(&b'/').to_vec(),
        }
    }

    pub(crate) fn file_name(&self) -> &[u8] {
        let segs = self.path.split(|&c| c == b'/').filter(|e| e.len() > 0).collect::<Vec<&[u8]>>();
        if segs.len() > 0 {
            segs[segs.len()-1]
        } else {
            b""
        }
    }

    pub(crate) fn push_segment(&mut self, b: &[u8]) {
        if !self.is_collection() {
            self.path.push(b'/');
        }
        self.path.extend_from_slice(b);
    }

    pub(crate) fn get_mime_type_str(&self) -> &'static str {
        let name = self.file_name();
        let d = name.rsplitn(2, |&c| c == b'.').collect::<Vec<&[u8]>>();
        if d.len() > 1 {
            if let Ok(ext) = std::str::from_utf8(d[0]) {
                if let Some(t) = mime_guess::get_mime_type_str(ext) {
                    return t;
                }
            }
        }
        "application/octet-stream"
    }
}

