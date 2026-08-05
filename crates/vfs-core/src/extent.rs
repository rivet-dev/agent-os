#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClassifiedFileExtent {
    pub start: u64,
    pub end: u64,
    pub unwritten: bool,
}

pub(crate) fn classified_file_extent_at<A, U>(
    allocated: A,
    unwritten: U,
    wanted: usize,
) -> Option<ClassifiedFileExtent>
where
    A: IntoIterator<Item = (u64, u64)>,
    U: Iterator<Item = (u64, u64)> + Clone,
{
    let mut observed = 0usize;
    for (start, end) in allocated {
        let mut cursor = start;
        for (unwritten_start, unwritten_end) in unwritten.clone() {
            if unwritten_end <= cursor || unwritten_start >= end {
                continue;
            }
            if cursor < unwritten_start {
                if observed == wanted {
                    return Some(ClassifiedFileExtent {
                        start: cursor,
                        end: unwritten_start.min(end),
                        unwritten: false,
                    });
                }
                observed = observed.saturating_add(1);
            }
            let overlap_start = cursor.max(unwritten_start);
            let overlap_end = end.min(unwritten_end);
            if overlap_start < overlap_end {
                if observed == wanted {
                    return Some(ClassifiedFileExtent {
                        start: overlap_start,
                        end: overlap_end,
                        unwritten: true,
                    });
                }
                observed = observed.saturating_add(1);
                cursor = overlap_end;
            }
            if cursor == end {
                break;
            }
        }
        if cursor < end {
            if observed == wanted {
                return Some(ClassifiedFileExtent {
                    start: cursor,
                    end,
                    unwritten: false,
                });
            }
            observed = observed.saturating_add(1);
        }
    }
    None
}

pub(crate) fn sector_byte_ranges<I>(
    extents: I,
    size: u64,
) -> impl Iterator<Item = (u64, u64)> + Clone
where
    I: Iterator<Item = (u64, u64)> + Clone,
{
    extents.filter_map(move |(start, end)| {
        let start = start.saturating_mul(512).min(size);
        let end = end.saturating_mul(512).min(size);
        (start < end).then_some((start, end))
    })
}

#[cfg(test)]
mod tests {
    use super::{classified_file_extent_at, ClassifiedFileExtent};

    #[test]
    fn classifies_index_without_collecting_output_extents() {
        let allocated = [(0, 2048), (3072, 4096)];
        let unwritten = [(512, 1024), (3072, 4096)];
        let expected = [
            ClassifiedFileExtent {
                start: 0,
                end: 512,
                unwritten: false,
            },
            ClassifiedFileExtent {
                start: 512,
                end: 1024,
                unwritten: true,
            },
            ClassifiedFileExtent {
                start: 1024,
                end: 2048,
                unwritten: false,
            },
            ClassifiedFileExtent {
                start: 3072,
                end: 4096,
                unwritten: true,
            },
        ];

        for (index, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                classified_file_extent_at(allocated.into_iter(), unwritten.into_iter(), index),
                Some(expected)
            );
        }
        assert_eq!(
            classified_file_extent_at(allocated, unwritten.into_iter(), expected.len()),
            None
        );
    }
}
