#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category { Photo, Document, Academic, Video, Other }

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Photo => "photo",
            Category::Document => "document",
            Category::Academic => "academic",
            Category::Video => "video",
            Category::Other => "other",
        }
    }

    pub fn from_db(s: &str) -> Category {
        match s {
            "photo" => Category::Photo,
            "document" => Category::Document,
            "academic" => Category::Academic,
            "video" => Category::Video,
            _ => Category::Other,
        }
    }

    /// Map a file extension (without dot, any case) to a category.
    pub fn from_extension(ext: &str) -> Category {
        match ext.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "heic" | "tiff" | "bmp" | "raw" | "cr2" | "nef" | "webp" => Category::Photo,
            "mp4" | "mov" | "avi" | "mkv" | "wmv" | "flv" | "webm" | "m4v" => Category::Video,
            "bib" | "tex" | "ipynb" | "csv" | "parquet" | "mat" | "r" => Category::Academic,
            "pdf" | "doc" | "docx" | "txt" | "md" | "rtf" | "odt" | "xls" | "xlsx" | "ppt" | "pptx" => Category::Document,
            _ => Category::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_extensions() {
        assert_eq!(Category::from_extension("JPG"), Category::Photo);
        assert_eq!(Category::from_extension("pdf"), Category::Document);
        assert_eq!(Category::from_extension("bib"), Category::Academic);
        assert_eq!(Category::from_extension("mp4"), Category::Video);
        assert_eq!(Category::from_extension("xyz"), Category::Other);
        assert_eq!(Category::from_extension(""), Category::Other);
    }

    #[test]
    fn db_roundtrip() {
        for c in [Category::Photo, Category::Document, Category::Academic, Category::Video, Category::Other] {
            assert_eq!(Category::from_db(c.as_str()), c);
        }
    }
}
