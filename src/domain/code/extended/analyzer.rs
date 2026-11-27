use crate::domain::code::comment_evidence::CommentEvidence;
use crate::domain::code::analyzer::{Document, FileAnalyzer, Measured};
use crate::domain::code::{Analysis, Language, extended, score};
use score::Input;
pub(in crate::domain::code) struct Objects(pub(in crate::domain::code) Language);
impl FileAnalyzer for Objects {
    fn comment_evidence(&self, _: Language, text: &str) -> CommentEvidence {
        CommentEvidence::objects(self.0, text)
    }

    fn analyze(&self, doc: &Document) -> Analysis {
        extended::objects::analyze(self.0, doc.text)
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        let mut base = None;
        let facts = extended::objects::analyze_scored(self.0, doc.text, Some(&mut base));
        Input::Source.measure(doc, facts, base, tests)
    }
}
pub(in crate::domain::code) struct ExtendedScripts(pub(in crate::domain::code) Language);
impl FileAnalyzer for ExtendedScripts {
    fn comment_evidence(&self, _: Language, text: &str) -> CommentEvidence {
        CommentEvidence::scripts(self.0, text)
    }

    fn analyze(&self, doc: &Document) -> Analysis {
        extended::scripts::analyze(self.0, doc.text)
    }
    fn measure(&self, doc: &Document, tests: bool) -> Measured {
        let (facts, base) = extended::scripts::scored(self.0, doc.text);
        Input::Source.measure(doc, facts, base, tests)
    }
}
