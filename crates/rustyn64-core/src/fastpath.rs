//! The fast path's **hand-off enumeration** and run report (ADR 0012 §2).
//!
//! ADR 0012 requires the differential gate to witness its own completion, and it
//! makes one demand of the *production* code to make that possible:
//!
//! > For that enumeration to mean anything, **bailing out must not be expressible
//! > any other way**: the fast path's only exit to the accurate scheduler is a typed
//! > reason — one enum, returned through one signature — so an ad-hoc early return in
//! > execution logic is a compile error rather than an uncounted path.
//!
//! This module is that enum, plus the set it is reported in.
//!
//! # How an ad-hoc exit becomes a compile error
//!
//! Two mechanisms, and neither is a convention anyone has to remember:
//!
//! 1. **The block executor returns `FastRunReport`, never `()`.** A bare `return;`
//!    in a function with that return type does not compile. Adding a new exit
//!    therefore forces the author to say *which* exit it is, in the type system.
//! 2. **`BailOut` and `BailOut::ALL` are generated from one list** by the
//!    `bail_reasons!` macro below. The count of reasons cannot be stated
//!    independently of the enum, which is exactly the property ADR 0012 asks for
//!    (it explicitly leaves the *technique* open — "an exhaustive match, a generated
//!    table, a `strum`-style derive"). A gate whose expected count is a literal
//!    drifts the moment the suite grows, which converts the witness into decoration.
//!
//! # Why this is a return value and not a counter on `System`
//!
//! Two constraints meet here. ADR 0011 §4's save-state mode marker falls due the
//! moment the fast path stores state of its own, and a bail-out tally would be
//! exactly that. ADR 0011 §6 forbids a seam that exists only for tests being
//! compiled into a release build. A return value satisfies both: it adds no field,
//! so the save-state layout is untouched, and it adds no *branch*, so the released
//! path carries nothing its own tests invented. It is the function reporting what it
//! did to the caller that asked it to do it.
//!
//! # Why a bitset rather than a list
//!
//! The gate's requirement is **variant coverage, not a fixture census** (ADR 0012
//! §2): every reason must be reached at least once, and how many times is not
//! information the gate asserts on. A `u32` bitset is `Copy`, allocation-free,
//! `no_std`-clean, and cannot encode the count that nobody should be counting.
//!
//! Present only with `--features fast-scheduler`, like the path it describes.

/// Declare the bail-out reasons **once**, and derive everything from that one list.
///
/// The enum, the exhaustive `ALL` table, and the human description all come from
/// the same expansion, so a new reason cannot be added to one without the others.
/// That is the ADR 0012 property: the number of reasons is not stateable
/// independently of the enum.
macro_rules! bail_reasons {
    ($( $(#[$attr:meta])* $name:ident => $desc:literal ),+ $(,)?) => {
        /// Why the fast path handed a stretch of the timeline back to the accurate
        /// scheduler (ADR 0011 §6).
        ///
        /// **Not `#[non_exhaustive]`, deliberately.** The differential gate is an
        /// integration test and therefore a downstream crate, and
        /// `#[non_exhaustive]` would force it to carry a wildcard arm — which is
        /// precisely the silent coverage hole ADR 0012 §2 exists to close. The
        /// gate must be able to match this exhaustively so that a new reason with
        /// no fixture is a **build failure**, not a quiet pass.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum BailOut {
            $( $(#[$attr])* $name, )+
        }

        impl BailOut {
            /// Every reason, generated from the same list as the enum itself.
            ///
            /// A hand-maintained array here would be the "literal expected count"
            /// ADR 0012 §2 rules out. This one cannot drift: adding a variant
            /// without adding it here is not possible, because there is only one
            /// place to add it.
            pub const ALL: &'static [Self] = &[ $( Self::$name, )+ ];

            /// One line naming what the fast path could not do itself.
            ///
            /// Used in gate failure messages, where "variant 3 was never reached"
            /// is a worse thing to read at 2 a.m. than the reason spelled out.
            #[must_use]
            pub const fn describe(self) -> &'static str {
                match self { $( Self::$name => $desc, )+ }
            }

            /// This reason's bit position in a [`BailOutSet`].
            #[must_use]
            const fn bit(self) -> u32 {
                // `ALL`'s order is the macro's order, and `as` on a fieldless enum
                // yields its discriminant, which is that same index.
                self as u32
            }
        }
    };
}

bail_reasons! {
    /// Fewer than one whole edge period remained before the target, so the
    /// remainder — and the `master_ticks = target` landing — ran on the accurate
    /// scheduler.
    ///
    /// This is the only reason the fast path has today, and saying so is the
    /// point: the machinery exists **before** the backend that will need it, so
    /// that the backend cannot add an uncounted exit. See `docs/scheduler.md`.
    PartialPeriodTail => "a partial edge period remained; the accurate scheduler ran the tail",
}

const _: () = {
    assert!(
        BailOut::ALL.len() <= u32::BITS as usize,
        "BailOutSet is a u32 bitset; a 33rd reason needs a wider set, not a wrap"
    );
};

/// The set of reasons observed during one fast-path call.
///
/// A set rather than a count, because ADR 0012 §2 asserts **coverage**: "one reason
/// may need several fixtures to reach it under different machine states, and one
/// fixture may trip a reason many times". Counting events would make a second
/// fixture for an already-covered reason look like progress and would say nothing
/// about the reason nobody wrote a fixture for.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct BailOutSet(u32);

/// Names the reasons, because the derived `BailOutSet(1)` is unreadable in exactly
/// the place this type is read: a gate failure message.
impl core::fmt::Debug for BailOutSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut set = f.debug_set();
        for &reason in BailOut::ALL {
            if self.contains(reason) {
                set.entry(&reason);
            }
        }
        set.finish()
    }
}

impl BailOutSet {
    /// The empty set — no hand-off happened.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Record that `reason` was reached.
    pub const fn insert(&mut self, reason: BailOut) {
        self.0 |= 1 << reason.bit();
    }

    /// Was `reason` reached?
    #[must_use]
    pub const fn contains(self, reason: BailOut) -> bool {
        self.0 & (1 << reason.bit()) != 0
    }

    /// Did any hand-off happen at all?
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Everything either set reached — how a gate accumulates across fixtures.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// What one call to the fast path actually did.
///
/// Both fields are witnesses ADR 0012 §2 names as suite-wide failure conditions —
/// *"the fast path never engaged"* and *"no bail-out boundary was reached"* — and
/// both would otherwise look exactly like success.
///
/// `blocks` is the engagement half: a fast path that quietly deferred everything to
/// the accurate scheduler would agree with it perfectly and prove nothing, which is
/// literally what #224 shipped on purpose while the gate was being built. It is a
/// count and not a flag because "engaged once over a whole suite" and "engaged
/// throughout" are worth telling apart in a failure message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[must_use = "the run report is ADR 0012's completion witness; a caller that drops \
              it should say so explicitly with `let _ =`"]
pub struct FastRunReport {
    /// Whole edge periods the block executor ran itself.
    pub blocks: u64,
    /// Reasons the accurate scheduler was handed a stretch.
    pub bailed: BailOutSet,
}

impl FastRunReport {
    /// Did the block executor run at all?
    #[must_use]
    pub const fn engaged(self) -> bool {
        self.blocks > 0
    }

    /// Accumulate another call's report: blocks **add**, reasons **union**.
    ///
    /// The rule lives here rather than at each call site because the two halves
    /// fail differently. A caller that adds the counts and forgets the union still
    /// compiles, still reports engagement, and **under-reports coverage** — which
    /// is the one thing ADR 0012 §2's witness exists to catch, so a bug in the
    /// accumulation would disable the check rather than trip it.
    ///
    /// `saturating_add` because a block count is a diagnostic: a wrapped total
    /// could read as zero and turn "engaged constantly" into "never engaged". It
    /// cannot be reached in practice — `u64::MAX` blocks is 6 x 2^64 master
    /// ticks — but saturating is the failure that stays interpretable.
    ///
    /// (No `#[must_use]` here: the returned type already carries one, and clippy's
    /// `double_must_use` rejects a second without a distinct message.)
    pub const fn merge(self, other: Self) -> Self {
        Self {
            blocks: self.blocks.saturating_add(other.blocks),
            bailed: self.bailed.union(other.bailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BailOut, BailOutSet, FastRunReport};

    /// Every reason must round-trip through the set at its own bit.
    ///
    /// Written over `BailOut::ALL` rather than over named variants so it keeps
    /// covering reasons added later — the same reason the gate derives its expected
    /// count from the enum instead of a literal.
    #[test]
    fn every_reason_round_trips_and_no_two_share_a_bit() {
        for (i, &reason) in BailOut::ALL.iter().enumerate() {
            let mut set = BailOutSet::new();
            assert!(set.is_empty(), "a fresh set contains nothing");
            set.insert(reason);
            assert!(set.contains(reason), "{reason:?} was inserted");
            assert!(!set.is_empty(), "a set with a reason in it is not empty");

            // Distinctness: no other reason may appear in a set holding only this
            // one. Two variants sharing a bit would make the coverage witness
            // report a reason that was never reached, which is worse than reporting
            // none.
            for &other in BailOut::ALL {
                if other != reason {
                    assert!(
                        !set.contains(other),
                        "{other:?} shares a bit with {reason:?}"
                    );
                }
            }
            assert!(
                !reason.describe().is_empty(),
                "reason {i} has no description"
            );
        }
    }

    /// `union` must accumulate, which is the only operation the gate performs.
    #[test]
    fn union_accumulates_every_reason() {
        let all = BailOut::ALL.iter().fold(BailOutSet::new(), |acc, &r| {
            let mut one = BailOutSet::new();
            one.insert(r);
            acc.union(one)
        });
        for &reason in BailOut::ALL {
            assert!(all.contains(reason), "union dropped {reason:?}");
        }
    }

    /// A report with no blocks has not engaged, whatever else it says.
    #[test]
    fn engagement_is_about_blocks_not_bailouts() {
        let mut bailed = BailOutSet::new();
        bailed.insert(BailOut::PartialPeriodTail);
        let deferred = FastRunReport { blocks: 0, bailed };
        assert!(
            !deferred.engaged(),
            "a run that executed no block has not engaged, even having handed off"
        );
        assert!(
            FastRunReport {
                blocks: 1,
                ..deferred
            }
            .engaged()
        );
    }
}
