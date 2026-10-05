//! Sorted disjoint half-open ranges used by payload liveness analyses.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Intervals(pub(crate) Vec<(usize, usize)>);
impl Intervals {
    pub(crate) fn insert(&mut self, mut low: usize, mut high: usize) {
        if low == high {
            return;
        }
        let first = self.0.partition_point(|&(_, end)| end < low);
        let mut last = first;
        while last < self.0.len() && self.0[last].0 <= high {
            low = low.min(self.0[last].0);
            high = high.max(self.0[last].1);
            last += 1;
        }
        self.0.splice(first..last, [(low, high)]);
    }
    pub(crate) fn remove(&mut self, low: usize, high: usize) {
        if low == high {
            return;
        }
        let mut result = Vec::new();
        for &(start, end) in &self.0 {
            if end <= low || start >= high {
                result.push((start, end));
                continue;
            }
            if start < low {
                result.push((start, low));
            }
            if end > high {
                result.push((high, end));
            }
        }
        self.0 = result;
    }
    pub(crate) fn intersects(&self, low: usize, high: usize) -> bool {
        low < high && self.0.iter().any(|&(start, end)| start < high && end > low)
    }
}
