[out:json][timeout:300];

// Alps bounding box (South, West, North, East)
// Covers: Austria, Switzerland, Italy, France, Southern Germany
// Latitude: 44°N to 48°N
// Longitude: 5°E to 16°E

// Query all ski areas (ways and relations)
(
  way["landuse"="winter_sports"]["name"](44,5,48,16);
  relation["landuse"="winter_sports"]["name"](44,5,48,16);
  relation["site"="piste"]["name"](44,5,48,16);
);

// Output ways with full geometry, relations with bounds and members
out bb geom;

// For site=piste relations, also output their member ways with geometry
relation._["site"="piste"];
way(r);
out geom;
