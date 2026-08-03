use crate::DirtyRect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocOpKind {
    Stroke,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub struct DocOp {
    pub layer: usize,
    pub dirty: DirtyRect,
    pub kind: DocOpKind,
    pub seq: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DocOpJournal {
    ops: Vec<DocOp>,
    next_seq: u64,
}

impl DocOpJournal {
    pub fn push(&mut self, layer: usize, dirty: DirtyRect, kind: DocOpKind) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.ops.push(DocOp {
            layer,
            dirty,
            kind,
            seq,
        });
        seq
    }

    pub fn since(&self, seq: u64) -> &[DocOp] {
        let idx = self.ops.partition_point(|op| op.seq < seq);
        &self.ops[idx..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_seq_increments() {
        let mut journal = DocOpJournal::default();
        let dirty = DirtyRect {
            x0: 1,
            y0: 2,
            x1: 3,
            y1: 4,
        };

        let a = journal.push(0, dirty, DocOpKind::Stroke);
        let b = journal.push(0, dirty, DocOpKind::Other);

        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(journal.since(1).len(), 1);
        assert_eq!(journal.since(1)[0].seq, 1);
    }
}
