//! Distances between vectors. The loops run over eight independent sums, which the
//! compiler turns into SIMD for the target CPU (AVX2 and AVX-512 on x86-64, NEON on
//! ARM) without code per instruction set.

/// How near two vectors are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Metric {
    /// The cosine of their angle (the default: what text embeddings are trained for).
    #[default]
    Cosine,
    /// Their dot product (for vectors normalised by the model, or trained for it).
    Dot,
    /// Their Euclidean distance.
    L2,
}

impl Metric {
    /// The metric named `name` (`cosine`, `dot`, `l2`; any case).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "cosine" => Some(Self::Cosine),
            "dot" | "inner" | "ip" => Some(Self::Dot),
            "l2" | "euclidean" => Some(Self::L2),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::Dot => "dot",
            Self::L2 => "l2",
        }
    }

    /// The score a query reports for a distance the indexes order by: the cosine
    /// similarity, the dot product, or the Euclidean distance.
    pub fn score(self, distance: f32) -> f32 {
        match self {
            Self::Cosine => 1.0 - distance,
            Self::Dot => -distance,
            Self::L2 => distance.max(0.0).sqrt(),
        }
    }
}

/// Lanes summed apart.
const LANES: usize = 8;

/// The dot product of `a` and `b` (of one length).
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut sums = [0.0f32; LANES];
    let chunks = n / LANES;
    for c in 0..chunks {
        let (x, y) = (&a[c * LANES..][..LANES], &b[c * LANES..][..LANES]);
        for lane in 0..LANES {
            sums[lane] += x[lane] * y[lane];
        }
    }
    let mut sum: f32 = sums.iter().sum();
    for i in chunks * LANES..n {
        sum += a[i] * b[i];
    }
    sum
}

/// The squared Euclidean distance between `a` and `b` (of one length).
pub fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut sums = [0.0f32; LANES];
    let chunks = n / LANES;
    for c in 0..chunks {
        let (x, y) = (&a[c * LANES..][..LANES], &b[c * LANES..][..LANES]);
        for lane in 0..LANES {
            let d = x[lane] - y[lane];
            sums[lane] += d * d;
        }
    }
    let mut sum: f32 = sums.iter().sum();
    for i in chunks * LANES..n {
        let d = a[i] - b[i];
        sum += d * d;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernels_agree_with_the_definitions() {
        for n in [0, 1, 7, 8, 9, 31, 768] {
            let a: Vec<f32> = (0..n).map(|i| (i as f32 * 0.37).sin()).collect();
            let b: Vec<f32> = (0..n).map(|i| (i as f32 * 0.11).cos()).collect();
            let naive_dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            let naive_l2: f32 = a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum();
            assert!((dot(&a, &b) - naive_dot).abs() <= 1e-3 * (1.0 + naive_dot.abs()));
            assert!((squared_l2(&a, &b) - naive_l2).abs() <= 1e-3 * (1.0 + naive_l2));
        }
    }
}
