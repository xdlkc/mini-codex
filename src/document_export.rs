//! Bounded, offline Word export of the validated local Pages schema.
//! No HTML import, external image fetch, macros, or executable package parts.
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Cursor, Write};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

type Result<T> = std::result::Result<T, String>;
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAX_XML: usize = 15 * 1024 * 1024;
const MAX_MEDIA: usize = 20 * 1024 * 1024;
const MAX_PACKAGE: usize = 20 * 1024 * 1024;
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";

pub(crate) struct Export {
    pub bytes: Vec<u8>,
    pub warnings: Vec<String>,
}

fn escape(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t'
            | '\n'
            | '\r'
            | '\u{20}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}' => out.push(c),
            _ => out.push('\u{fffd}'),
        }
    }
    out
}

fn children(node: &Value) -> &[Value] {
    node["content"].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn attr<'a>(node: &'a Value, key: &str) -> &'a str {
    node["attrs"][key].as_str().unwrap_or("")
}
fn span(node: &Value, key: &str) -> usize {
    node["attrs"][key].as_u64().unwrap_or(1) as usize
}

#[derive(Clone, Default)]
struct Paragraph {
    indent: usize,
    quote: bool,
    code: bool,
    align: String,
    bold: bool,
    numbering: Option<(u32, usize)>,
    prefix: &'static str,
}

struct Image {
    path: String,
    mime: String,
    bytes: Vec<u8>,
    rel: usize,
    width: u64,
    height: u64,
}

struct Writer<F> {
    image_reader: F,
    xml: String,
    relationships: Vec<String>,
    lists: Vec<(String, String)>,
    images: Vec<Image>,
    image_ids: HashMap<String, usize>,
    links: HashMap<String, usize>,
    media_bytes: usize,
    drawing_id: u32,
    warnings: Vec<String>,
}

impl<F: FnMut(&str) -> Result<(Value, Vec<u8>)>> Writer<F> {
    fn warn(&mut self, text: &str) {
        if !self.warnings.iter().any(|warning| warning == text) {
            self.warnings.push(text.into());
        }
    }
    fn bounded(&self) -> Result<()> {
        if self.xml.len() > MAX_XML || self.relationships.len() > 2000 || self.lists.len() > 1000 {
            return Err("Word export exceeds its document complexity or 15 MiB XML limit".into());
        }
        Ok(())
    }
    fn relation(&mut self, kind: &str, target: &str, external: bool) -> usize {
        let id = self.relationships.len() + 3;
        self.relationships.push(format!(
            "<Relationship Id=\"rId{id}\" Type=\"{R}/{kind}\" Target=\"{}\"{}/>",
            escape(target),
            if external {
                " TargetMode=\"External\""
            } else {
                ""
            }
        ));
        id
    }
    fn run(&mut self, text: &str, marks: &[Value], context: &Paragraph) {
        let has = |name: &str| marks.iter().any(|mark| mark["type"] == name);
        let mut properties = String::new();
        if context.code || has("code") {
            properties.push_str("<w:rFonts w:ascii=\"Courier New\" w:hAnsi=\"Courier New\"/>");
        }
        if context.bold || has("bold") {
            properties.push_str("<w:b/>");
        }
        if has("italic") {
            properties.push_str("<w:i/>");
        }
        if has("strike") {
            properties.push_str("<w:strike/>");
        }
        let link = marks
            .iter()
            .find(|mark| mark["type"] == "link")
            .map(|mark| attr(mark, "href"))
            .filter(|href| !href.is_empty());
        if let Some(href) = link {
            let id = if let Some(id) = self.links.get(href) {
                *id
            } else {
                let id = self.relation("hyperlink", href, true);
                self.links.insert(href.into(), id);
                id
            };
            self.xml
                .push_str(&format!("<w:hyperlink r:id=\"rId{id}\">"));
            properties.push_str("<w:color w:val=\"0563C1\"/>");
        }
        if link.is_some() || has("underline") {
            properties.push_str("<w:u w:val=\"single\"/>");
        }
        if context.code || has("code") {
            properties.push_str("<w:shd w:fill=\"F3F3F3\"/>");
        }
        self.xml.push_str("<w:r><w:rPr>");
        self.xml.push_str(&properties);
        self.xml.push_str("</w:rPr>");
        // w:t must not silently collapse whitespace, tabs or line breaks.
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut segment = String::new();
        for c in normalized.chars() {
            if c == '\n' || c == '\t' {
                if !segment.is_empty() {
                    self.xml.push_str(&format!(
                        "<w:t xml:space=\"preserve\">{}</w:t>",
                        escape(&segment)
                    ));
                    segment.clear();
                }
                self.xml
                    .push_str(if c == '\n' { "<w:br/>" } else { "<w:tab/>" });
            } else {
                segment.push(c);
            }
        }
        if !segment.is_empty() {
            self.xml.push_str(&format!(
                "<w:t xml:space=\"preserve\">{}</w:t>",
                escape(&segment)
            ));
        }
        self.xml.push_str("</w:r>");
        if link.is_some() {
            self.xml.push_str("</w:hyperlink>");
        }
    }
    fn paragraph(&mut self, node: &Value, context: &Paragraph) -> Result<()> {
        self.xml.push_str("<w:p><w:pPr>");
        let name = node["type"].as_str().unwrap_or("");
        if name == "heading" {
            let level = node["attrs"]["level"].as_u64().unwrap_or(1);
            self.xml
                .push_str(&format!("<w:pStyle w:val=\"Heading{level}\"/>"));
        }
        if let Some((id, level)) = context.numbering {
            self.xml.push_str(&format!(
                "<w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{id}\"/></w:numPr>"
            ));
        }
        if context.quote {
            self.xml.push_str("<w:pBdr><w:left w:val=\"single\" w:sz=\"12\" w:space=\"8\" w:color=\"CCCCCC\"/></w:pBdr>");
        }
        if context.code {
            self.xml.push_str("<w:shd w:fill=\"F3F3F3\"/>");
        }
        if context.indent > 0 || context.quote {
            self.xml.push_str(&format!(
                "<w:ind w:left=\"{}\"/>",
                360 * (context.indent + usize::from(context.quote))
            ));
        }
        if !context.align.is_empty() {
            self.xml
                .push_str(&format!("<w:jc w:val=\"{}\"/>", escape(&context.align)));
        }
        self.xml.push_str("</w:pPr>");
        if !context.prefix.is_empty() {
            self.run(context.prefix, &[], context);
        }
        for child in children(node) {
            let marks = child["marks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            match child["type"].as_str().unwrap_or("") {
                "text" => self.run(child["text"].as_str().unwrap_or(""), marks, context),
                "hardBreak" => self.xml.push_str("<w:r><w:br/></w:r>"),
                "inlineMath" => {
                    self.warn("Equations are exported as LaTeX text, not editable Word equations.");
                    self.run(&format!("${}$", attr(child, "latex")), marks, context);
                }
                _ => return Err("Unsupported inline content in Word export".into()),
            }
        }
        self.xml.push_str("</w:p>");
        self.bounded()
    }
    fn list(&mut self, node: &Value, context: &Paragraph) -> Result<()> {
        let ordered = node["type"] == "orderedList";
        let tasks = node["type"] == "taskList";
        let level = context.indent.min(8);
        let id = self.lists.len() as u32 + 1;
        if !tasks {
            let format = if !ordered {
                "bullet"
            } else {
                match attr(node, "type") {
                    "a" => "lowerLetter",
                    "A" => "upperLetter",
                    "i" => "lowerRoman",
                    "I" => "upperRoman",
                    _ => "decimal",
                }
            };
            let start = if ordered {
                node["attrs"]["start"].as_u64().unwrap_or(1)
            } else {
                1
            };
            let levels = (0..9).map(|index| {
                let text = if ordered { format!("%{}.", index + 1) } else { "•".into() };
                format!("<w:lvl w:ilvl=\"{index}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{format}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:tabs><w:tab w:val=\"num\" w:pos=\"{}\"/></w:tabs><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr></w:lvl>", 360 * (index + 1), 360 * (index + 1))
            }).collect::<String>();
            self.lists.push((format!("<w:abstractNum w:abstractNumId=\"{id}\"><w:multiLevelType w:val=\"multilevel\"/>{levels}</w:abstractNum>"), format!("<w:num w:numId=\"{id}\"><w:abstractNumId w:val=\"{id}\"/><w:lvlOverride w:ilvl=\"{level}\"><w:startOverride w:val=\"{start}\"/></w:lvlOverride></w:num>")));
        }
        for item in children(node) {
            for (index, block) in children(item).iter().enumerate() {
                let mut next = context.clone();
                next.indent += 1;
                next.numbering = if index == 0 && !tasks {
                    Some((id, level))
                } else {
                    None
                };
                next.prefix = if index == 0 && tasks {
                    if item["attrs"]["checked"] == true {
                        "☒ "
                    } else {
                        "☐ "
                    }
                } else {
                    ""
                };
                self.block(block, &next)?;
            }
        }
        Ok(())
    }
    fn image(&mut self, node: &Value) -> Result<()> {
        let key = attr(node, "attachmentId");
        let index = if let Some(index) = self.image_ids.get(key) {
            *index
        } else {
            if self.images.len() >= 1000 {
                return Err("Word export supports up to 1000 images".into());
            }
            let (metadata, bytes) = (self.image_reader)(key).map_err(|_| "A document image is missing or unreadable. Restore it in the file library before exporting Word.".to_string())?;
            self.media_bytes += bytes.len();
            if self.media_bytes > MAX_MEDIA {
                return Err("Word export images exceed 20 MiB".into());
            }
            let (mime, extension) = match metadata["mimeType"].as_str() {
                Some("image/png") => ("image/png", "png"),
                Some("image/jpeg") => ("image/jpeg", "jpg"),
                Some("image/webp") => ("image/png", "png"),
                _ => return Err("Unsupported image in Word export".into()),
            };
            let bytes = if metadata["mimeType"] == "image/webp" {
                let image =
                    image::load_from_memory(&bytes).map_err(|_| "Could not read document image")?;
                let mut png = Cursor::new(Vec::new());
                image
                    .write_to(&mut png, image::ImageFormat::Png)
                    .map_err(|_| "Could not encode document image")?;
                self.media_bytes = self.media_bytes - bytes.len() + png.get_ref().len();
                if self.media_bytes > MAX_MEDIA {
                    return Err("Word export images exceed 20 MiB".into());
                }
                png.into_inner()
            } else {
                bytes
            };
            let index = self.images.len();
            let path = format!("media/image{}.{}", index + 1, extension);
            let rel = self.relation("image", &path, false);
            self.images.push(Image {
                path,
                mime: mime.into(),
                bytes,
                rel,
                width: metadata["width"].as_u64().unwrap_or(640),
                height: metadata["height"].as_u64().unwrap_or(480),
            });
            self.image_ids.insert(key.into(), index);
            index
        };
        let image = &self.images[index];
        let width = node["attrs"]["width"]
            .as_u64()
            .unwrap_or(image.width)
            .max(1);
        let height = node["attrs"]["height"]
            .as_u64()
            .unwrap_or_else(|| image.height.max(1) * width / image.width.max(1))
            .max(1);
        // Fit A4's printable area while retaining the requested aspect ratio.
        let factor = (624.0 / width as f64).min(900.0 / height as f64).min(1.0);
        let cx = (width as f64 * factor * 9525.0) as u64;
        let cy = (height as f64 * factor * 9525.0) as u64;
        self.drawing_id += 1;
        self.xml.push_str(&format!("<w:p><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{}\" name=\"{}\" descr=\"{}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"0\" name=\"{}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"rId{}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>", self.drawing_id, escape(attr(node, "name")), escape(attr(node, "alt")), escape(attr(node, "name")), image.rel));
        self.bounded()
    }
    fn table(&mut self, node: &Value, context: &Paragraph) -> Result<()> {
        // Materialize continuation cells for rowspan. DOCX requires a vMerge
        // continuation in each row, unlike ProseMirror's sparse cell arrays.
        let rows = children(node);
        let mut grid: Vec<Vec<Option<(usize, usize)>>> = vec![Vec::new(); rows.len()];
        let mut width = 0;
        for (r, row) in rows.iter().enumerate() {
            let mut c = 0;
            for (index, cell) in children(row).iter().enumerate() {
                while grid[r].get(c).is_some_and(Option::is_some) {
                    c += 1;
                }
                for slots in grid.iter_mut().skip(r).take(span(cell, "rowspan")) {
                    slots.resize(slots.len().max(c + span(cell, "colspan")), None);
                    slots[c..c + span(cell, "colspan")].fill(Some((r, index)));
                }
                c += span(cell, "colspan");
            }
            width = width.max(grid[r].len());
        }
        let mut widths = vec![0_u64; width];
        for (r, row) in rows.iter().enumerate() {
            for c in 0..width {
                if let Some((origin_row, index)) = grid[r][c] {
                    if origin_row == r {
                        let cell = &children(row)[index];
                        if let Some(values) = cell["attrs"]["colwidth"].as_array() {
                            let first = grid[r]
                                .iter()
                                .position(|slot| *slot == Some((origin_row, index)))
                                .unwrap_or(c);
                            widths[c] = values[c - first].as_u64().unwrap_or(0) * 15;
                        }
                    }
                }
            }
        }
        let fallback = 9360 / width.max(1) as u64;
        for value in &mut widths {
            if *value == 0 {
                *value = fallback;
            }
        }
        let total: u64 = widths.iter().sum();
        if total > 9360 {
            for value in &mut widths {
                *value = (*value * 9360 / total).max(1);
            }
        }
        self.xml
            .push_str("<w:tbl><w:tblPr><w:tblW w:w=\"5000\" w:type=\"pct\"/><w:tblBorders>");
        for border in ["top", "left", "bottom", "right", "insideH", "insideV"] {
            self.xml.push_str(&format!(
                "<w:{border} w:val=\"single\" w:sz=\"4\" w:color=\"D0D0D0\"/>"
            ));
        }
        self.xml
            .push_str("</w:tblBorders><w:tblLayout w:type=\"fixed\"/></w:tblPr><w:tblGrid>");
        for value in &widths {
            self.xml.push_str(&format!("<w:gridCol w:w=\"{value}\"/>"));
        }
        self.xml.push_str("</w:tblGrid>");
        for (r, row) in grid.iter().enumerate() {
            self.xml.push_str("<w:tr>");
            let mut c = 0;
            while c < width {
                let (origin_row, index) = row[c].ok_or("Invalid table grid in Word export")?;
                let cell = &children(&rows[origin_row])[index];
                let colspan = span(cell, "colspan");
                self.xml.push_str(&format!(
                    "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>",
                    widths[c..c + colspan].iter().sum::<u64>()
                ));
                if colspan > 1 {
                    self.xml
                        .push_str(&format!("<w:gridSpan w:val=\"{colspan}\"/>"));
                }
                if span(cell, "rowspan") > 1 {
                    self.xml.push_str(if origin_row == r {
                        "<w:vMerge w:val=\"restart\"/>"
                    } else {
                        "<w:vMerge/>"
                    });
                }
                if cell["type"] == "tableHeader" {
                    self.xml.push_str("<w:shd w:fill=\"F2F2F2\"/>");
                }
                self.xml.push_str("</w:tcPr>");
                if origin_row == r {
                    let mut next = context.clone();
                    next.align = attr(cell, "align").into();
                    next.bold = cell["type"] == "tableHeader";
                    next.numbering = None;
                    next.prefix = "";
                    for block in children(cell) {
                        self.block(block, &next)?;
                    }
                    if children(cell)
                        .last()
                        .is_some_and(|block| block["type"] == "table")
                    {
                        self.xml.push_str("<w:p/>");
                    }
                } else {
                    self.xml.push_str("<w:p/>");
                }
                self.xml.push_str("</w:tc>");
                c += colspan;
            }
            self.xml.push_str("</w:tr>");
            self.bounded()?;
        }
        self.xml.push_str("</w:tbl>");
        Ok(())
    }
    fn block(&mut self, node: &Value, context: &Paragraph) -> Result<()> {
        match node["type"].as_str().unwrap_or("") {
            "doc" => for child in children(node) { self.block(child, context)?; },
            "paragraph" | "heading" => self.paragraph(node, context)?,
            "codeBlock" => { let mut next = context.clone(); next.code = true; self.paragraph(node, &next)?; },
            "blockquote" => { let mut next = context.clone(); next.quote = true; for child in children(node) { self.block(child, &next)?; } },
            "bulletList" | "orderedList" | "taskList" => self.list(node, context)?,
            "horizontalRule" => self.xml.push_str("<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:color=\"CCCCCC\"/></w:pBdr></w:pPr></w:p>"),
            "table" => self.table(node, context)?,
            "image" => self.image(node)?,
            "fileAttachment" => {
                self.warn("File blocks include their names only. Download file originals separately from the file library.");
                self.xml.push_str("<w:p>"); self.run(&format!("[File: {}]", attr(node, "name")), &[], context); self.xml.push_str("</w:p>");
            }
            "blockMath" => {
                self.warn("Equations are exported as LaTeX text, not editable Word equations.");
                self.xml.push_str("<w:p>"); self.run(&format!("$$\n{}\n$$", attr(node, "latex")), &[], context); self.xml.push_str("</w:p>");
            }
            "workBlock"=>{
                self.warn("Local Work blocks export their text and references. Interactive visualizations export their HTML source.");
                self.xml.push_str("<w:p>");self.run(&format!("{}\n{}\n{}\n{}",attr(node,"title"),attr(node,"text"),attr(node,"targetId"),attr(node,"html")),&[],context);self.xml.push_str("</w:p>");
            }
            _ => return Err("Unsupported block in Word export".into()),
        }
        self.bounded()
    }
}

pub(crate) fn docx(version: &Value) -> Result<Export> {
    export(version, crate::attachments::read_image_original)
}

fn export(
    version: &Value,
    image_reader: impl FnMut(&str) -> Result<(Value, Vec<u8>)>,
) -> Result<Export> {
    let mut writer = Writer {
        image_reader,
        xml: String::new(),
        relationships: Vec::new(),
        lists: Vec::new(),
        images: Vec::new(),
        image_ids: HashMap::new(),
        links: HashMap::new(),
        media_bytes: 0,
        drawing_id: 0,
        warnings: Vec::new(),
    };
    writer.xml = format!("{DECL}<w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:body>");
    if version["format"] == "richtext" {
        crate::documents::validate_body(&version["body"])?;
        writer.block(&version["body"], &Paragraph::default())?;
    } else {
        writer.warn("Source Markdown is exported as literal text. Convert to rich text to export its formatting.");
        if version["content"]
            .as_str()
            .unwrap_or("")
            .chars()
            .any(|c| c == '\0' || (c.is_control() && !matches!(c, '\t' | '\n' | '\r')))
        {
            writer.warn("XML-incompatible control characters in source text are replaced with the replacement character.");
        }
        for line in version["content"].as_str().unwrap_or("").split('\n') {
            writer.xml.push_str("<w:p>");
            writer.run(
                line,
                &[],
                &Paragraph {
                    code: true,
                    ..Default::default()
                },
            );
            writer.xml.push_str("</w:p>");
            writer.bounded()?;
        }
    }
    if version["commentCount"].as_u64().unwrap_or(0) > 0 {
        writer.warn("Local comments and revision history are not included in the Word file.");
    }
    writer.xml.push_str("<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1273\" w:right=\"1273\" w:bottom=\"1273\" w:left=\"1273\"/></w:sectPr></w:body></w:document>");
    writer.bounded()?;
    let mut types = format!("{DECL}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>");
    for image in &writer.images {
        types.push_str(&format!(
            "<Override PartName=\"/word/{}\" ContentType=\"{}\"/>",
            image.path, image.mime
        ));
    }
    types.push_str("</Types>");
    let rels = format!("{DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R}/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId2\" Type=\"{R}/numbering\" Target=\"numbering.xml\"/>{}</Relationships>", writer.relationships.join(""));
    let numbering = format!(
        "{DECL}<w:numbering xmlns:w=\"{W}\">{}{}</w:numbering>",
        writer
            .lists
            .iter()
            .map(|list| list.0.as_str())
            .collect::<String>(),
        writer
            .lists
            .iter()
            .map(|list| list.1.as_str())
            .collect::<String>()
    );
    let mut styles = format!("{DECL}<w:styles xmlns:w=\"{W}\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:eastAsia=\"Microsoft YaHei\"/><w:sz w:val=\"22\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"276\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>");
    for level in 1..=6 {
        styles.push_str(&format!("<w:style w:type=\"paragraph\" w:styleId=\"Heading{level}\"><w:name w:val=\"heading {level}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"120\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{}\"/></w:rPr></w:style>", level - 1, [36, 30, 26, 24, 22, 22][level - 1]));
    }
    styles.push_str("</w:styles>");
    let core = format!("{DECL}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title><dc:creator>Mini ChatGPT</dc:creator></cp:coreProperties>", escape(version["title"].as_str().unwrap_or("Document")));
    let root_rels = format!("{DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R}/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>");
    let mut package = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (path, bytes) in [
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/document.xml", writer.xml.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("docProps/core.xml", core.as_bytes()),
    ] {
        package
            .start_file(path, options)
            .and_then(|_| package.write_all(bytes).map_err(Into::into))
            .map_err(|_| "Could not build Word file")?;
    }
    for image in &writer.images {
        package
            .start_file(format!("word/{}", image.path), options)
            .and_then(|_| package.write_all(&image.bytes).map_err(Into::into))
            .map_err(|_| "Could not write Word image")?;
    }
    let bytes = package
        .finish()
        .map_err(|_| "Could not finish Word file")?
        .into_inner();
    if bytes.len() > MAX_PACKAGE {
        return Err("Word export exceeds 20 MiB".into());
    }
    Ok(Export {
        bytes,
        warnings: writer.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Read;

    fn paragraph(text: &str) -> Value {
        json!({"type":"paragraph","content":[{"type":"text","text":text}]})
    }
    fn version(blocks: Vec<Value>) -> Value {
        json!({"title":"中文 <Word> 😀","format":"richtext","body":{"type":"doc","content":blocks}})
    }
    fn parts(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|index| {
                let mut file = zip.by_index(index).unwrap();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).unwrap();
                (file.name().into(), bytes)
            })
            .collect()
    }
    fn xml(parts: &HashMap<String, Vec<u8>>, path: &str) -> String {
        String::from_utf8(parts[path].clone()).unwrap()
    }
    fn no_images(_: &str) -> Result<(Value, Vec<u8>)> {
        panic!("Text export must never read attachments");
    }

    #[test]
    fn unicode_marks_links_and_xml_package_remain_readable_without_active_content() {
        let exported=export(&version(vec![json!({"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"中文 & <title> 😀"}]}), json!({"type":"paragraph","content":[{"type":"text","text":" a\tb\n链接 ","marks":[{"type":"bold"},{"type":"italic"},{"type":"underline"},{"type":"strike"},{"type":"link","attrs":{"href":"https://example.test/?a=1&b=2"}}]}]}), json!({"type":"codeBlock","content":[{"type":"text","text":"<script>literal</script>"}]})]), no_images).unwrap();
        assert!(exported.warnings.is_empty());
        let package = parts(&exported.bytes);
        assert_eq!(package.len(), 7);
        for (name, bytes) in &package {
            assert!(!name.contains("..") && !name.ends_with(".bin"));
            let mut reader = quick_xml::Reader::from_reader(bytes.as_slice());
            loop {
                match reader.read_event().unwrap() {
                    quick_xml::events::Event::Eof => break,
                    _ => {}
                }
            }
        }
        let text = xml(&package, "word/document.xml");
        assert!(text.contains("中文 &amp; &lt;title&gt; 😀"));
        assert!(
            text.contains("<w:b/>")
                && text.contains("<w:i/>")
                && text.contains("<w:strike/>")
                && text.contains("<w:u w:val=\"single\"/>")
        );
        assert!(
            text.contains("<w:tab/>")
                && text.contains("<w:br/>")
                && text.contains("xml:space=\"preserve\"")
        );
        assert!(!text.contains("<script>"));
        assert!(text.contains("w:pStyle w:val=\"Heading2\""));
        assert!(xml(&package, "word/_rels/document.xml.rels")
            .contains("Target=\"https://example.test/?a=1&amp;b=2\" TargetMode=\"External\""));
        assert!(xml(&package, "docProps/core.xml").contains("中文 &lt;Word&gt; 😀"));
        let bad = version(vec![
            json!({"type":"paragraph","content":[{"type":"text","text":"bad","marks":[{"type":"link","attrs":{"href":"file:///etc/passwd"}}]}]}),
        ]);
        assert!(export(&bad, no_images).is_err());
    }

    #[test]
    fn numbered_lists_restart_and_merged_tables_include_vertical_continuations() {
        let cell = |text: &str, cols, rows| json!({"type":"tableCell","attrs":{"colspan":cols,"rowspan":rows},"content":[paragraph(text)]});
        let table = json!({"type":"table","content":[{"type":"tableRow","content":[cell("MERGED",2,2),cell("RIGHT 1",1,1)]},{"type":"tableRow","content":[cell("RIGHT 2",1,1)]}]});
        let list = |start| json!({"type":"orderedList","attrs":{"start":start,"type":"a"},"content":[{"type":"listItem","content":[paragraph("item"),{"type":"bulletList","content":[{"type":"listItem","content":[paragraph("nested")]}]}]}]});
        let exported = export(&version(vec![list(3), table, list(7)]), no_images).unwrap();
        let package = parts(&exported.bytes);
        let document = xml(&package, "word/document.xml");
        let numbering = xml(&package, "word/numbering.xml");
        assert_eq!(document.matches("<w:gridCol ").count(), 3);
        assert_eq!(document.matches("<w:tc>").count(), 4);
        assert_eq!(document.matches("MERGED").count(), 1);
        assert_eq!(document.matches("<w:gridSpan w:val=\"2\"/>").count(), 2);
        assert!(
            document.contains("<w:vMerge w:val=\"restart\"/>") && document.contains("<w:vMerge/>")
        );
        assert!(
            numbering.contains("<w:startOverride w:val=\"3\"/>")
                && numbering.contains("<w:startOverride w:val=\"7\"/>")
        );
        assert!(
            numbering.rfind("</w:abstractNum>").unwrap()
                < numbering.find("<w:num w:numId=").unwrap()
        );
        assert!(numbering.contains("lowerLetter") && numbering.contains("bullet"));
    }

    #[test]
    fn images_are_embedded_once_webp_is_converted_and_missing_images_fail_export() {
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(3, 2)
            .write_to(&mut encoded, image::ImageFormat::WebP)
            .unwrap();
        let bytes = encoded.into_inner();
        let mut reads = 0;
        let image = json!({"type":"image","attrs":{"attachmentId":format!("att_{}","a".repeat(32)),"name":"../../image.webp","alt":"中文 & alt"}});
        let document = version(vec![image.clone(), image]);
        let exported = export(&document, |_| {
            reads += 1;
            Ok((
                json!({"mimeType":"image/webp","width":3,"height":2}),
                bytes.clone(),
            ))
        })
        .unwrap();
        assert_eq!(reads, 1);
        let package = parts(&exported.bytes);
        assert!(package.contains_key("word/media/image1.png") && package.len() == 8);
        assert_eq!(
            image::load_from_memory(&package["word/media/image1.png"])
                .unwrap()
                .width(),
            3
        );
        assert_eq!(
            xml(&package, "word/document.xml")
                .matches("r:embed=\"rId3\"")
                .count(),
            2
        );
        assert!(
            xml(&package, "word/_rels/document.xml.rels").contains("Target=\"media/image1.png\"")
        );
        assert!(export(&document, |_| Err("missing".into()))
            .err()
            .unwrap()
            .contains("missing or unreadable"));
        assert!(export(&document, |_| Ok((
            json!({"mimeType":"image/png"}),
            vec![0; MAX_MEDIA + 1]
        )))
        .err()
        .unwrap()
        .contains("20 MiB"));
    }

    #[test]
    fn representational_limits_are_explicit_and_source_text_never_becomes_markup() {
        let document = version(vec![
            json!({"type":"taskList","content":[{"type":"taskItem","attrs":{"checked":true},"content":[paragraph("done")]}]}),
            json!({"type":"blockMath","attrs":{"latex":"x^2"}}),
            json!({"type":"fileAttachment","attrs":{"attachmentId":format!("att_{}","b".repeat(32)),"name":"report.pdf"}}),
        ]);
        let exported = export(&document, no_images).unwrap();
        let package = parts(&exported.bytes);
        assert_eq!(exported.warnings.len(), 2);
        assert!(xml(&package, "word/document.xml").contains("☒ "));
        let source=export(&json!({"title":"source","format":"markdown","content":"# literal\n<script>text</script>\0"}),no_images).unwrap();
        assert_eq!(source.warnings.len(), 2);
        assert!(xml(&parts(&source.bytes), "word/document.xml")
            .contains("&lt;script&gt;text&lt;/script&gt;�"));
    }
}
