# Audit 5: the UI crawled at several screen sizes and densities

Unarr at main 77a39ce, fixture library with covers fetched from the mock.
36 pages (every main, settings and system page, four series pages) on five
screens: desktop at pixel ratio 1, 2 and 3, iPhone 13, iPad.

- About 1,100 responses seen per screen; none with a status of 400 or more,
  no failed request, no page error, no console error, no broken image.
- Cover sizes the UI asked for: poster-250/500/750 on the index,
  poster-500/1000/1500 on a series page, fanart.jpg. All answered 200; the
  sizes nothing makes (750, 1000, 1500) come from the fallback added today.

Not covered: anything behind a click (modals, saving, interactive search,
manual import, the banner and overview index views), and requests only real
data triggers.
