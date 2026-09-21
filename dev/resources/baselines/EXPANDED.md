# The baseline corpus is deliberately oversized

`baseline_tests::SIZES` renders 8, 12, 32 and 64 px instead of 32 px alone. That takes the
reference tree from 5,754 PNGs to 23,016, and 90 MB against a 41 MB pack. The repository has been
allowed to carry that while the raster refactor is in progress, because small sizes are where DDA
stepping differences show up and one size cannot see them.

Recorded on 2026-09-21. Restore condition: the integer DDA has landed and its output is trusted.
Then cut `SIZES` back, delete the references for the sizes that go, and delete this file. The
guard in `baseline_tests::expanded_corpus_is_declared` fails in both directions, so neither half
of that can be forgotten.

Shrinking does not have to mean going back to one size. 32 px alone was too little to catch what
the round 1 review found; two sizes may be the right resting place. Decide it with the numbers
from a run, not from this note.
