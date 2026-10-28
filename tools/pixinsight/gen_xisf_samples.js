// gen_xisf_samples.js -- write a matrix of XISF sample files with PixInsight's
// own encoder, for manual reader smoke tests (docs/superpowers/plans/
// 2026-10-28-formats-to-mpp-xisf-rev2.md, Phase 0.4).
//
// Run headless:
//
//   /opt/PixInsight/bin/PixInsight.sh -n --automation-mode --no-splash \
//      --no-startup-scripts --no-startup-check-updates \
//      -r="tools/pixinsight/gen_xisf_samples.js,<out-dir>[,<mode>[,<panel>]]" \
//      --force-exit
//
// <mode>  small (default) | big | bigmixed | defaults | panel | all
//         | recompress (then one or more panel paths)
// <panel> a solved XISF panel to re-save compressed (mode panel/all)
//
// Every compressed file has an uncompressed twin ("__none__none") written
// from the same in-memory image: decoding the compressed file must give the
// twin's pixels exactly. manifest-<mode>.json records each file's settings.
// Pixels come from a seeded xorshift PRNG, so reruns are reproducible.

#include <pjsr/ColorSpace.jsh>
#include <pjsr/SampleType.jsh>

"use strict";

const CODECS = [ "none", "zlib", "zlib+sh", "lz4", "lz4+sh", "lz4hc", "lz4hc+sh", "zstd", "zstd+sh" ];
const CHECKSUMS = [ "sha1", "sha256", "sha512" ];

// Odd sizes so rows and planes never land on round boundaries.
const W = 601, H = 401;

// LZ4's input limit (LZ4_MAX_INPUT_SIZE = 0x7E000000) is the smallest of the
// PI 1.9.5 codecs, and PI sets no subblock size of its own, so only a block
// larger than that gets subblocks. 14000 x 14000 x 3 Float32 = 2.35 GB.
const BIG = 14000;
// Rows at the bottom of the last channel filled with noise, so the trailing
// LZ4 subblock (~240 MB) does not compress and is stored raw (c = u).
const BIG_NOISE_ROWS = 4400;

// This PJSR engine has no Object.assign.
function merge( a, b )
{
   for ( let k in b )
      a[k] = b[k];
   return a;
}

function Rng( seed )
{
   let s = seed >>> 0 || 1;
   this.next = function()
   {
      s ^= s << 13; s >>>= 0;
      s ^= s >>> 17;
      s ^= s << 5; s >>>= 0;
      return s / 4294967296;
   };
}

const FORMATS = {
   u8:  { bits:  8, float: false },
   u16: { bits: 16, float: false },
   u32: { bits: 32, float: false },
   f32: { bits: 32, float: true },
   f64: { bits: 64, float: true }
};

function newImage( w, h, color, fmt )
{
   let f = FORMATS[fmt];
   return new Image( w, h, color ? 3 : 1, color ? ColorSpace_RGB : ColorSpace_Gray,
                     f.bits, f.float ? SampleType_Real : SampleType_Integer );
}

// pattern(x, y, c, rng) -> normalized sample in [0,1]
function fill( img, pattern, rng )
{
   let w = img.width, h = img.height;
   let row = new Float64Array( w );
   for ( let c = 0; c < img.numberOfChannels; ++c )
      for ( let y = 0; y < h; ++y )
      {
         for ( let x = 0; x < w; ++x )
            row[x] = pattern( x, y, c, rng );
         img.setSamples( row, new Rect( 0, y, w, y+1 ), c );
      }
}

// Smooth gradient plus a little seeded noise: compresses well, not trivially.
function smooth( w, h )
{
   return function( x, y, c, rng )
   {
      let v = 0.15 + 0.6*(x/w)*(0.5 + 0.5*y/h) + 0.05*c + 0.01*rng.next();
      return Math.min( 1, Math.max( 0, v ) );
   };
}

function noise( x, y, c, rng )
{
   return rng.next();
}

function hints( codec, checksum )
{
   let h = (codec == "none") ? "no-compression" : "compression-codec " + codec + " compression-level 0";
   h += (checksum == "none") ? " no-checksums" : " checksums " + checksum;
   return h;
}

function fileName( stem, codec, checksum )
{
   return stem + "__" + codec.replace( "+", "-" ) + "__" + checksum + ".xisf";
}

function writeXisf( path, img, hintString, extra )
{
   let F = new FileFormat( "XISF", false/*toRead*/, true/*toWrite*/ );
   let f = new FileFormatInstance( F );
   if ( f.isNull )
      throw new Error( "Unable to instantiate XISF format" );
   if ( !f.create( path, hintString ) )
      throw new Error( "Unable to create " + path );
   let d = new ImageDescription;
   d.bitsPerSample = img.bitsPerSample;
   d.ieeefpSampleFormat = img.isReal;
   if ( !f.setOptions( d ) )
      throw new Error( "Unable to set options for " + path );
   if ( extra )
      extra( f );
   if ( !f.writeImage( img ) )
      throw new Error( "Unable to write " + path );
   f.close();
}

function Manifest( outDir, mode )
{
   this.entries = [];
   this.add = function( e ) { this.entries.push( e ); console.writeln( "<raw>" + e.file + "</raw>" ); };
   this.save = function()
   {
      let doc = {
         generator: "tools/pixinsight/gen_xisf_samples.js",
         pixinsight: format( "%d.%d.%d-%d", coreVersionMajor, coreVersionMinor, coreVersionRelease, coreVersionBuild ),
         created: (new Date).toISOString(),
         mode: mode,
         files: this.entries
      };
      File.writeTextFile( outDir + "/manifest-" + mode + ".json", JSON.stringify( doc, null, 1 ) );
   };
}

// Write one image under each (codec, checksum) pair, plus its uncompressed twin.
function writeMatrix( outDir, M, stem, img, info, pairs )
{
   let reference = fileName( stem, "none", "none" );
   writeXisf( outDir + "/" + reference, img, hints( "none", "none" ) );
   M.add( merge( { file: reference, codec: "none", checksum: "none", reference: reference }, info ) );
   for ( let p of pairs )
   {
      if ( p[0] == "none" && p[1] == "none" )
         continue;
      let name = fileName( stem, p[0], p[1] );
      writeXisf( outDir + "/" + name, img, hints( p[0], p[1] ) );
      M.add( merge( { file: name, codec: p[0], checksum: p[1], reference: reference }, info ) );
   }
}

function genSmall( outDir )
{
   let M = new Manifest( outDir, "small" );
   let allCodecs = CODECS.map( c => [ c, "none" ] );

   // Every sample format x color space x codec, compressible content.
   let seed = 1;
   for ( let fmt in FORMATS )
      for ( let color of [ false, true ] )
      {
         let img = newImage( W, H, color, fmt );
         fill( img, smooth( W, H ), new Rng( seed++ ) );
         let cs = color ? "rgb" : "gray";
         writeMatrix( outDir, M, fmt + "_" + cs + "_smooth", img,
                      { sampleFormat: fmt, colorSpace: cs, pattern: "smooth", width: W, height: H }, allCodecs );
      }

   // Incompressible content: PCL stores subblocks that do not shrink raw
   // (compressed length == uncompressed length) -- the Rev 2 rule.
   for ( let [fmt, color] of [ [ "f32", false ], [ "u16", true ] ] )
   {
      let img = newImage( W, H, color, fmt );
      fill( img, noise, new Rng( seed++ ) );
      let cs = color ? "rgb" : "gray";
      writeMatrix( outDir, M, fmt + "_" + cs + "_noise", img,
                   { sampleFormat: fmt, colorSpace: cs, pattern: "noise", width: W, height: H }, allCodecs );
   }

   // Checksums: digest over stored (compressed) bytes, so cross with codecs.
   for ( let [fmt, color] of [ [ "f32", true ], [ "u16", false ] ] )
   {
      let img = newImage( W, H, color, fmt );
      fill( img, smooth( W, H ), new Rng( seed++ ) );
      let cs = color ? "rgb" : "gray";
      let pairs = [];
      for ( let codec of [ "none", "lz4", "zstd+sh" ] )
         for ( let sum of CHECKSUMS )
            pairs.push( [ codec, sum ] );
      writeMatrix( outDir, M, fmt + "_" + cs + "_cksum", img,
                   { sampleFormat: fmt, colorSpace: cs, pattern: "smooth", width: W, height: H }, pairs );
   }

   // Two images in one unit (baseline decoders must read multiple Images).
   try
   {
      let a = newImage( W, H, false, "u16" );
      fill( a, smooth( W, H ), new Rng( seed++ ) );
      let b = newImage( W, H, true, "f32" );
      fill( b, noise, new Rng( seed++ ) );
      let name = "multi_u16gray_f32rgb__zstd-sh__sha256.xisf";
      let F = new FileFormat( "XISF", false, true );
      let f = new FileFormatInstance( F );
      if ( !f.create( outDir + "/" + name, hints( "zstd+sh", "sha256" ) + " image-ids first,second" ) )
         throw new Error( "create" );
      for ( let img of [ a, b ] )
      {
         let d = new ImageDescription;
         d.bitsPerSample = img.bitsPerSample;
         d.ieeefpSampleFormat = img.isReal;
         if ( !f.setOptions( d ) || !f.writeImage( img ) )
            throw new Error( "write" );
      }
      f.close();
      M.add( { file: name, codec: "zstd+sh", checksum: "sha256", images: [ "first:u16:gray", "second:f32:rgb" ],
               width: W, height: H } );
   }
   catch ( e )
   {
      console.warningln( "** Multi-image unit not written: " + e.message );
   }

   M.save();
}

function genBig( outDir )
{
   let M = new Manifest( outDir, "big" );
   let img = newImage( BIG, BIG, true, "f32" );
   let sm = smooth( BIG, BIG );
   fill( img, function( x, y, c, rng )
   {
      return (c == 2 && y >= BIG - BIG_NOISE_ROWS) ? rng.next() : sm( x, y, c, rng );
   }, new Rng( 1000 ) );
   writeMatrix( outDir, M, "big_f32_rgb_tailnoise", img,
                { sampleFormat: "f32", colorSpace: "rgb", pattern: "smooth+tailnoise", width: BIG, height: BIG,
                  note: "lz4+sh exceeds LZ4_MAX_INPUT_SIZE: 2 compressed subblocks. The 1% noise defeats " +
                        "unshuffled lz4/lz4hc entirely, so PI writes those uncompressed" },
                [ [ "lz4", "none" ], [ "lz4+sh", "sha256" ], [ "lz4hc", "sha1" ], [ "zstd+sh", "sha512" ] ] );
   M.save();
}

// Quantized, noise-free head + noise tail: unshuffled lz4 compresses the
// first subblock and stores the second raw (c = u) -- a mixed subblock list.
function genBigMixed( outDir )
{
   let M = new Manifest( outDir, "bigmixed" );
   let img = newImage( BIG, BIG, true, "f32" );
   fill( img, function( x, y, c, rng )
   {
      if ( c == 2 && y >= BIG - BIG_NOISE_ROWS )
         return rng.next();
      return Math.round( (0.15 + 0.6*(x/BIG)*(0.5 + 0.5*y/BIG) + 0.05*c)*256 )/256;
   }, new Rng( 2000 ) );
   writeMatrix( outDir, M, "big_f32_rgb_mixed", img,
                { sampleFormat: "f32", colorSpace: "rgb", pattern: "quantized+tailnoise", width: BIG, height: BIG,
                  note: "unshuffled lz4/lz4hc: subblock 1 compressed, subblock 2 raw (c = u)" },
                [ [ "lz4", "none" ], [ "lz4hc", "sha256" ] ] );
   M.save();
}

// Read a panel's image, FITS keywords and properties.
function readPanel( panelPath )
{
   let F = new FileFormat( "XISF", true/*toRead*/, false/*toWrite*/ );
   let f = new FileFormatInstance( F );
   let d = f.open( panelPath, "" );
   if ( d.length != 1 )
      throw new Error( "Expected one image in " + panelPath );
   let img = new Image( 1, 1, 1, ColorSpace_Gray, d[0].bitsPerSample,
                        d[0].ieeefpSampleFormat ? SampleType_Real : SampleType_Integer );
   if ( !f.readImage( img ) )
      throw new Error( "Unable to read " + panelPath );
   let keywords = f.keywords;
   let properties = [];
   for ( let p of f.imageProperties )
   {
      let v = f.readImageProperty( p[0] );
      if ( v != null )
         properties.push( { id: p[0], type: p[1], value: v } );
   }
   f.close();
   return {
      image: img,
      copyMeta: function( out )
      {
         out.keywords = keywords;
         for ( let p of properties )
            out.writeImageProperty( p.id, p.value, p.type );
      }
   };
}

// Re-save a real solved panel: AstrometricSolution properties (large attached
// F64Matrix blocks, compressed too) plus FITS keywords.
function genPanel( outDir, panelPath )
{
   let M = new Manifest( outDir, "panel" );
   let src = readPanel( panelPath );
   let img = src.image, copyMeta = src.copyMeta;
   let stem = "panel_" + File.extractName( panelPath ).replace( /[^A-Za-z0-9_-]+/g, "_" );
   let reference = stem + "__none__none.xisf";
   writeXisf( outDir + "/" + reference, img, hints( "none", "none" ), copyMeta );
   M.add( { file: reference, codec: "none", checksum: "none", reference: reference, source: panelPath } );
   for ( let p of [ [ "zstd+sh", "sha256" ], [ "lz4hc", "sha512" ], [ "zlib+sh", "sha1" ] ] )
   {
      let name = fileName( stem, p[0], p[1] );
      writeXisf( outDir + "/" + name, img, hints( p[0], p[1] ), copyMeta );
      M.add( { file: name, codec: p[0], checksum: p[1], reference: reference, source: panelPath } );
   }
   M.save();
}

// What the format writes when given no hints: the installation's (and this
// user's) default XISF output options.
function genDefaults( outDir )
{
   let M = new Manifest( outDir, "defaults" );
   let img = newImage( W, H, true, "f32" );
   fill( img, smooth( W, H ), new Rng( 3000 ) );
   writeXisf( outDir + "/defaults_f32_rgb_smooth.xisf", img, "" );
   M.add( { file: "defaults_f32_rgb_smooth.xisf", hints: "", sampleFormat: "f32", colorSpace: "rgb", width: W, height: H } );
   M.save();
}

// Re-save each given panel as zstd+sh with SHA-256 checksums (the way WBPP
// writes masters) under its own file name in outDir: a compressed copy of a
// real mosaic for end-to-end runs.
function genRecompress( outDir, panelPaths )
{
   let M = new Manifest( outDir, "recompress" );
   for ( let panelPath of panelPaths )
   {
      let src = readPanel( panelPath );
      let name = File.extractName( panelPath ) + File.extractExtension( panelPath );
      writeXisf( outDir + "/" + name, src.image, hints( "zstd+sh", "sha256" ), src.copyMeta );
      M.add( { file: name, codec: "zstd+sh", checksum: "sha256", source: panelPath } );
      src.image.free();
   }
   M.save();
}

function main()
{
   if ( typeof jsArguments == "undefined" || jsArguments.length < 1 )
      throw new Error( "usage: -r=\"gen_xisf_samples.js,<out-dir>[,small|big|bigmixed|defaults|panel|all[,<panel.xisf>]]\"" );
   let outDir = jsArguments[0];
   let mode = (jsArguments.length > 1) ? jsArguments[1] : "small";
   if ( !File.directoryExists( outDir ) )
      File.createDirectory( outDir, true );
   let T = new ElapsedTime;
   if ( mode == "small" || mode == "all" )
      genSmall( outDir );
   if ( mode == "big" || mode == "all" )
      genBig( outDir );
   if ( mode == "bigmixed" || mode == "all" )
      genBigMixed( outDir );
   if ( mode == "defaults" || mode == "all" )
      genDefaults( outDir );
   if ( mode == "recompress" )
      genRecompress( outDir, jsArguments.slice( 2 ) );
   if ( mode == "panel" || mode == "all" )
   {
      if ( jsArguments.length < 3 )
         throw new Error( "mode " + mode + " needs a panel path" );
      genPanel( outDir, jsArguments[2] );
   }
   console.noteln( "gen_xisf_samples: done in " + T.text );
}

try
{
   main();
}
catch ( e )
{
   console.criticalln( "gen_xisf_samples: " + e.message );
   // Leave a marker so the shell side can tell failure from success.
   if ( typeof jsArguments != "undefined" && jsArguments.length > 0 )
      File.writeTextFile( jsArguments[0] + "/FAILED.txt", e.message + "\n" );
}
