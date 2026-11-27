use super::*;

pub(super) struct SourceCapture<'a> {
    plan: &'a CapturePlan<'a>,
    keep_graph: bool,
}
pub(super) type Reader<'a> =
    dyn Fn(usize, &ObservedPath, Option<usize>) -> io::Result<Option<String>> + Sync + 'a;

impl<'a> SourceCapture<'a> {
    pub fn new(plan: &'a CapturePlan<'a>, keep_graph: bool) -> Self {
        Self { plan, keep_graph }
    }
}

enum LocalFacts {
    Deferred,
    Reuse(usize, Option<Box<Parsed>>),
    Analyze,
}

impl SourceCapture<'_> {
    fn forwards_sources(&self) -> bool {
        self.plan.reads.forward_known && self.keep_graph
    }

    pub fn run(
        &self,
        obs: &mut [ObservedPath],
        table: &mut CaptureTable,
        prepared: Option<usize>,
    ) -> io::Result<()> {
        let mut jobs = capture::Jobs::new(self.plan.budget.capture_credit);
        let previous = self.plan.inputs.predecessor.previous;
        let direct = previous.filter(|_| self.forwards_sources() && self.plan.reads.share_markdown);
        let remaining = obs.iter_mut().zip(table.rows()).enumerate()
            .filter(|(index, _)| prepared != Some(*index));
        for (index, (o, slot)) in remaining {
            let prior = previous.and_then(|_| self.plan.reuse[index]);
            self.schedule(&mut jobs, (index, o, slot), (prior, direct))?;
        }
        jobs.run_steps(self.plan.budget.width, |work| match work {
            Work::Read(index, path, slot, prior) => self.capture(index, path, slot, prior),
            Work::Analyze(mut job) => Ok((!job.advance(None)).then_some(Work::Analyze(job))),
        })
    }
    fn schedule<'a>(
        &self,
        jobs: &mut capture::Jobs<Work<'a>>,
        (index, file, slot): (usize, &'a mut ObservedPath, capture_table::Row<'a>),
        (prior, direct): (Option<usize>, Option<&dyn Previous>),
    ) -> io::Result<()> {
        if let Some((previous, id)) = direct.zip(prior) {
            slot.store(self.forward(index, file, previous, id)?);
        } else {
            jobs.push(file.stamp[2], Work::Read(index, file, slot, prior));
        }
        Ok(())
    }

    fn forward(
        &self,
        index: usize,
        observed: &ObservedPath,
        previous: &dyn Previous,
        id: usize,
    ) -> io::Result<Captured> {
        let (prior, blob) =
            (self.plan.inputs.identify)(index, observed, None, PriorRow::Unchanged(id))?;
        Ok(Captured {
            blob,
            source: None,
            local: Some(previous.local(id, false, false)?),
            prior,
        })
    }

    fn reused_local(
        &self,
        id: usize,
        refresh_markdown: bool,
        regenerated_md: Option<Box<Parsed>>,
    ) -> io::Result<Local> {
        let p = self.plan.inputs.predecessor.previous.unwrap();
        let mut local = p.local(id, !self.keep_graph, self.plan.reads.decode_markdown)?;
        if refresh_markdown {
            local.markdown = regenerated_md;
        }
        Ok(local)
    }

    fn capture<'a>(
        &self,
        index: usize,
        o: &'a mut ObservedPath,
        slot: capture_table::Row<'a>,
        prior: Option<usize>,
    ) -> io::Result<Option<Work<'a>>> {
        // A graph-preserving edit has no consumer for the retained
        // source bodies until their bytes reach the new segment.
        let refresh_markdown = self.plan.reads.refresh_markdown && markdown::is_markdown(&o.path);
        let forwarded = self.forwards_sources() && prior.is_some() && !refresh_markdown;
        let body = self.read_body(index, o, prior, forwarded)?;
        let row = self.plan.paths.row(index, prior);
        let (prior, blob) = (self.plan.inputs.identify)(index, o, body.as_deref(), row)?;
        let same_path = prior.filter(|&id| row.ordinal() == Some(id));
        let facts = self.local_facts(&o.path, body.as_deref(), same_path, (forwarded, refresh_markdown));
        let source = body
            .map(|text| self.plan.budget.pool.retain(text))
            .transpose()?;
        let mut capture = Captured {
            blob,
            source,
            local: None,
            prior,
        };
        match facts {
            LocalFacts::Deferred => {}
            LocalFacts::Reuse(id, markdown) => {
                capture.local = Some(self.reused_local(id, refresh_markdown, markdown)?);
            }
            LocalFacts::Analyze => return Ok(slot.analyze(&o.path, capture)),
        }
        slot.store(capture);
        Ok(None)
    }

    fn read_body(
        &self,
        index: usize,
        observed: &mut ObservedPath,
        prior: Option<usize>,
        forwarded: bool,
    ) -> io::Result<Option<String>> {
        match (forwarded, observed.captured.take()) {
            (true, _) => Ok(None),
            (false, Some(text)) => Ok(text),
            (false, None) => (self.plan.inputs.read)(index, observed, prior),
        }
    }

    fn local_facts(
        &self,
        path: &str,
        body: Option<&str>,
        same_path: Option<usize>,
        (forwarded, refresh_markdown): (bool, bool),
    ) -> LocalFacts {
        if !forwarded
            && self
                .plan
                .config
                .requires_analysis(path, same_path)
        {
            return LocalFacts::Deferred;
        }
        let Some(id) = same_path else {
            return LocalFacts::Analyze;
        };
        // Parse replacement Markdown before the pool can spill the body.
        let markdown = body
            .filter(|_| refresh_markdown)
            .map(|text| Box::new(Parsed::parse(path, text)));
        LocalFacts::Reuse(id, markdown)
    }
}
