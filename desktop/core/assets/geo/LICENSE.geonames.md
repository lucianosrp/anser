# GeoNames (CC BY 4.0)

`cities.tsv` is derived from the **GeoNames** `cities15000` dump
(https://download.geonames.org/export/dump/), © GeoNames
(https://www.geonames.org), licensed under the Creative Commons Attribution 4.0
International License (https://creativecommons.org/licenses/by/4.0/).

Changes: kept populated places with 100,000+ people (city sections dropped),
trimmed to country code, name (the ASCII name, plus the UTF-8 name and a few
local names such as Roma or Milano), latitude and longitude rounded to 0.01°.

The data is provided "as is", without warranty or any representation of
accuracy, timeliness or completeness.

Regenerate with `cargo run -p finance-core --example geo_assets` (see the
example's header for the inputs).
