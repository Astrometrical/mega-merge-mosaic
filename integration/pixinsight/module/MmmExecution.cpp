// MmmExecution.cpp -- see MmmExecution.h. Spec section 10.1 execution flow.

#include "MmmExecution.h"

#ifdef _WIN32
#  ifndef WIN32_LEAN_AND_MEAN
#    define WIN32_LEAN_AND_MEAN
#  endif
#  ifndef NOMINMAX
#    define NOMINMAX  // keep windows.h from defining min()/max() macros
#  endif
#  include <windows.h>
#else
#  include <dlfcn.h>
#  include <unistd.h>
#endif

#include <atomic>
#include <filesystem>
#include <string>
#include <vector>

#include <pcl/Array.h>
#include <pcl/Console.h>
#include <pcl/Exception.h>
#include <pcl/File.h>
#include <pcl/FileFormat.h>
#include <pcl/FileFormatInstance.h>
#include <pcl/Image.h>
#include <pcl/ImageDescription.h>
#include <pcl/ImageVariant.h>
#include <pcl/ImageWindow.h>
#include <pcl/MetaModule.h>
#include <pcl/Property.h>
#include <pcl/View.h>

#include "AstrometryProps.h"
#include "ImageWindowCollector.h"
#include "MmmParameters.h"
#include "MmmProcess.h"
#include "MmmVersion.h"
#include "ViewPanelSource.h"
#include "mmm_host.h"
#include "third_party/json.hpp"

namespace pcl
{

using nlohmann::json;

// ----------------------------------------------------------------------------
// Module-scope state for cooperative cancellation. run_blend() publishes the
// live host here for the duration of a run so the Console abort handler
// (ConsoleProgress::on_progress) can reach it; both Host::cancel() and the
// atomic pointer are safe to touch from another thread (mmm_host.h).
// ----------------------------------------------------------------------------

namespace
{

std::atomic<mmm::Host*> s_activeHost{ nullptr };
std::atomic<uint64_t>   s_runCounter{ 0 };

// A copy of the instance's parameters. run_blend() (the sole friend of
// MmmBlendInstance) snapshots the private members into this so the internal
// helpers below need no friendship of their own.
struct Params
{
   Array<String> viewIds;
   Array<String> filePaths;
   pcl_enum      inputSelect;
   std::string   sessionDir;
   int32         feather;
   pcl_enum      blendMode;
   int32         flatten;
   bool          flattenEnabled;
   bool          defectVeto;
   bool          seamMap;
   int32         surfaceOrder;
   pcl_enum      gainMode;
   int32         bandRows;
};

// Fixed slot-pool sizing (PROTOCOL.md section 9): input_slots >= worker thread
// count + 1 avoids stalling fetches; output_slots = 2 is ample. Matches the
// golden tests' SlotLayout{.,8,2}.
constexpr uint32_t kInputSlots  = 8;
constexpr uint32_t kOutputSlots = 2;

// --- Progress + cooperative abort ------------------------------------------
//
// The host invokes on_progress() synchronously on this (the ExecuteGlobal)
// thread between worker frames, so it is safe to touch the Console here.
// Progress is Console-only (the interface no longer mirrors it): we render an
// in-place, single-line bar per stage, pump the GUI event queue so a queued
// Cancel-button click is delivered, and poll the Console's own Abort button;
// either path calls Host::cancel(). This is the synchronous v1 (spec section
// 15): the tool window is not repainted mid-run beyond ProcessEvents() pumps.
//
// Deliberately NOT pursuing a pcl::Thread background-execution rewrite to make
// the whole tool window interactive during the blend -- this was considered and
// rejected, not merely deferred:
//   - It is non-idiomatic for a PixInsight process. Core processes such as
//     ImageIntegration run synchronously with Console progress plus a working
//     abort, which is exactly the model here.
//   - It is blocked by PCL's main-thread affinity for View/ImageWindow access:
//     DriveHost's serve loop reads source views (ViewPanelSource) and builds the
//     output ImageWindow (ImageWindowCollector), both of which must happen on
//     the GUI thread. Moving the run onto a worker pcl::Thread would require
//     marshalling all of that back to the main thread anyway, for no gain over
//     the current ProcessEvents()-pumped Console/interface progress model.
// Progress visibility (reproject/analyze/blend Progress frames, see
// PROTOCOL.md section 6) and a clean Cancel (ProcessAborted, not a modal error)
// are the fixes that make this synchronous model feel responsive; revisit this
// note before reopening the background-thread idea.
//
// on_idle(): the Host additionally invokes this whenever the worker pipe has
// been quiet for ~Host::kIdleWaitMs (no frame arrived), so the event queue
// keeps getting pumped even during long silent stretches -- e.g. while the
// worker reads multi-GB panel files and emits no Progress frames. Without it
// the GUI thread parks in a blocking pipe read and Windows greys the window
// as "not responding" after ~5 s of unpumped events.
//
// The probe phases are pumped the same way: Host::probe_frame and
// Host::probe_panels take this observer too, and honor want_cancel() by
// killing the probe child (surfaced as mmm::HostCancelled -> ProcessAborted).
// Files mode in particular never opens a panel file on this thread -- the
// worker's --probe-panels does the whole metadata pass (header-only,
// parallel) in its own process, so even an 80-panel scan leaves the UI live.
class ConsoleProgress : public mmm::ProgressCallback
{
public:
   mmm::Host* host = nullptr;

   void on_progress( const std::string& stage, uint64_t done, uint64_t total ) override
   {
      // Friendly stage names; fall back to the wire string verbatim.
      String name;
      if ( stage == "reproject" )    name = "Reprojecting";
      else if ( stage == "analyze" ) name = "Analyzing";
      else if ( stage == "blend" )   name = "Blending";
      else                           name = String( stage.c_str() );

      if ( name != m_stage )
      {
         // Commit the previous stage's line before starting a new one.
         if ( !m_stage.IsEmpty() && m_lineLen > 0 )
            CommitLine();
         m_stage = name;
         m_lastPercent = -1;
         m_lineLen = 0;
      }

      if ( total > 0 )
      {
         int percent = int( 100*done/total );
         if ( percent != m_lastPercent )
         {
            m_lastPercent = percent;
            int fill = 24*percent/100;

            // Visible glyphs, assembled separately from markup so the erase
            // length can be counted exactly (tags/ANSI are zero-width).
            IsoString name8 = IsoString( m_stage );
            if ( name8.Length() < 13 )
               name8.Append( ' ', 13 - name8.Length() );
            IsoString bar;
            bar.Append( '#', fill );
            bar.Append( '-', 24 - fill );
            IsoString pct = IsoString().Format( "%3d%%", percent );

            String line = "<b>" + String( name8 ) + "</b> "
                          "\x1b[38;2;42;149;171m[" + String( bar ) + "]\x1b[39m " + String( pct );
            int visible = int( name8.Length() ) + 1 + 1 + 24 + 1 + 1 + int( pct.Length() );

            Redraw( line, visible );
            if ( percent == 100 )
               CommitLine();
         }
      }
      else
      {
         IsoString name8 = IsoString( m_stage );
         if ( name8.Length() < 13 )
            name8.Append( ' ', 13 - name8.Length() );
         IsoString count = IsoString().Format( "%llu", (unsigned long long)done );
         Redraw( "<b>" + String( name8 ) + "</b> " + String( count ),
                 int( name8.Length() ) + 1 + int( count.Length() ) );
      }

      // Deliver pending GUI events and honor the Console's own abort button --
      // this is the (only) cancellation path.
      PumpEventsAndPollAbort();
   }

   void on_idle() override
   {
      // Quiet pipe (see the class comment): keep the GUI responsive and the
      // abort button live while the worker is busy but silent. Called at
      // most every Host::kIdleWaitMs, so no extra throttling is needed.
      PumpEventsAndPollAbort();
   }

   bool want_cancel() override
   {
      // Polled by the Host probe helpers (probe_frame/probe_panels) after
      // each on_idle(): during a probe there is no live Host to cancel(), so
      // the abort is delivered by killing the probe child instead
      // (HostCancelled, which the Run* callers map to ProcessAborted).
      return m_abortSeen;
   }

private:

   void PumpEventsAndPollAbort()
   {
      Module->ProcessEvents();
      if ( m_console.AbortRequested() )
      {
         m_abortSeen = true;
         m_console.Abort();
         if ( host != nullptr )
            host->cancel();
      }
   }

   Console m_console;
   String  m_stage;
   int     m_lastPercent = -1;
   int     m_lineLen = 0;   // visible glyphs currently on the in-progress line
   bool    m_abortSeen = false;   // sticky: Console abort observed this run

   // Erase the previously drawn glyphs with backspaces (the console's <bsp>
   // deletes a glyph; <bol>+write inserts, so \r cannot be used), then draw.
   // Invariant: between Redraw calls of a stage, this class owns the current
   // console line; any other Console write in that window would be partially
   // erased by the next call's backspaces.
   void Redraw( const String& line, int visible )
   {
      String s = "<end>";
      if ( m_lineLen > 0 )
         s.Append( String( '\b', size_type( m_lineLen ) ) );
      s += line;
      m_console.Write( s );
      m_lineLen = visible;
   }

   void CommitLine()
   {
      m_console.WriteLn();
      m_lineLen = 0;
   }
};

// Shameless branding at the start of every run: teal Astrometrical chevron +
// name/version/tagline. ASCII only; ANSI truecolor for the teal.
void WriteBanner()
{
   const String teal  = "\x1b[38;2;42;149;171m";
   const String reset = "\x1b[39m";
   Console c;
   c.WriteLn( "<end><cbr>" );
   c.WriteLn( teal + "     /\\" + reset );
   c.WriteLn( teal + "    /  \\" + reset + "     <b>Mega Merge Mosaic</b> version " MMM_VERSION_STRING );
   c.WriteLn( teal + "   / /\\ \\" + reset + "    Big mosaics, no big deal." );
   c.WriteLn( teal + "  /_/  \\_\\" + reset + "   (c) 2026 Astrometrical | https://astrometrical.com" );
   c.WriteLn();
}

// A PanelSource the worker never calls: Files mode reads panels off disk itself
// (PROTOCOL.md section 11), so no BandRequest is ever issued.
struct NullPanelSource : mmm::PanelSource
{
   bool fill_band( uint32_t, uint64_t, uint64_t, float* ) override { return false; }
};

// ----------------------------------------------------------------------------

// Directory containing this loaded module (.so/.dylib on POSIX, .dll on
// Windows), located via the address of a module symbol (PCL exposes no
// module-path API; PCL_API_REFERENCE.md section 8). POSIX uses dladdr; Windows
// uses GetModuleHandleExW(FROM_ADDRESS) + GetModuleFileNameW.
String ModuleDirectory()
{
#ifdef _WIN32
   HMODULE hmod = nullptr;
   if ( ::GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            reinterpret_cast<LPCWSTR>( &run_blend ), &hmod ) )
   {
      wchar_t buf[ 32768 ];
      DWORD n = ::GetModuleFileNameW( hmod, buf, DWORD( sizeof buf / sizeof buf[0] ) );
      if ( n > 0 && n < DWORD( sizeof buf / sizeof buf[0] ) )
      {
         // wchar_t is 16-bit on Windows == PCL String's UTF-16 code unit.
         String path( reinterpret_cast<const char16_t*>( buf ) );
         return File::ExtractDrive( path ) + File::ExtractDirectory( path );
      }
   }
   throw Error( "MegaMergeMosaic: could not determine the module path via GetModuleFileNameW()." );
#else
   Dl_info info;
   if ( dladdr( reinterpret_cast<void*>( &run_blend ), &info ) != 0 && info.dli_fname != nullptr )
   {
      String path( info.dli_fname );
      return File::ExtractDrive( path ) + File::ExtractDirectory( path );
   }
   throw Error( "MegaMergeMosaic: could not determine the module path via dladdr()." );
#endif
}

// Absolute path to the sibling mmm-ipc-worker binary; errors clearly if absent.
std::string ResolveWorkerPath()
{
   String dir = ModuleDirectory();
   if ( !dir.EndsWith( '/' ) )
      dir += '/';
#ifdef _WIN32
   String worker = dir + "mmm-ipc-worker.exe";
   if ( !File::Exists( worker ) )
      throw Error( "MegaMergeMosaic: worker binary not found next to the module: " + worker +
                   "\nBuild it with `cargo build --release -p mmm-ipc-worker` and copy "
                   "`mmm-ipc-worker.exe` beside mmm-pxm.dll." );
#else
   String worker = dir + "mmm-ipc-worker";
   if ( !File::Exists( worker ) )
      throw Error( "MegaMergeMosaic: worker binary not found next to the module: " + worker +
                   "\nBuild it with `cargo build --release -p mmm-ipc-worker` and copy "
                   "`mmm-ipc-worker` beside mmm-pxm.so." );
#endif
   return std::string( worker.ToUTF8().c_str() );
}

// Wire string for the InputSelect override enum (PROTOCOL.md section 11).
const char* InputSelectWireString( pcl_enum v )
{
   switch ( v )
   {
   case MmmInputSelectParameter::Aligned: return "Aligned";
   case MmmInputSelectParameter::Solved:  return "Solved";
   default:                               return "Auto";
   }
}

// Wire string for the blend algorithm enum (PROTOCOL.md section 6 BlendParamsWire).
const char* BlendModeWireString( pcl_enum v )
{
   switch ( v )
   {
   case MmmBlendModeParameter::Feather: return "feather";
   case MmmBlendModeParameter::TwoBand: return "twoband";
   default:                             return "pyramid";
   }
}

// Wire string for the gain-mode enum (PROTOCOL.md section 6 BlendParamsWire).
const char* GainModeWireString( pcl_enum v )
{
   switch ( v )
   {
   case MmmGainModeParameter::Unity: return "unity";
   default:                          return "fit";
   }
}

// Builds the BlendParamsWire object (PROTOCOL.md section 6) from the parameters.
json BuildParams( const Params& in )
{
   json p;
   p["feather_px"]    = double( in.feather );
   p["downsample"]    = uint32_t( 1 );   // parameter removed from UI; wire field is mandatory
   p["band_rows"]     = uint32_t( in.bandRows );
   p["mode"]          = BlendModeWireString( in.blendMode );
   p["roi"]           = nullptr;         // ROI removed from UI; wire field is mandatory
   p["defect_veto"]   = bool( in.defectVeto );
   p["seam_map"]      = bool( in.seamMap );
   if ( in.flattenEnabled )
      p["flatten"] = uint32_t( in.flatten );
   else
      p["flatten"] = nullptr;
   p["surface_order"] = uint32_t( in.surfaceOrder );
   p["gain"]          = GainModeWireString( in.gainMode );
   return p;
}

// A fresh, process-unique shm segment name (PROTOCOL.md section 2).
std::string MakeShmName()
{
   return "/mmm-pxm-" +
#ifdef _WIN32
          std::to_string( (unsigned long) ::GetCurrentProcessId() )
#else
          std::to_string( (unsigned long) ::getpid() )
#endif
          + "-" + std::to_string( s_runCounter.fetch_add( 1 ) );
}

// Best-effort recursive removal of an auto-created temp session directory.
// Only ever invoked on paths this module generated under the system temp dir;
// user-specified session directories are never touched.
void RemoveSessionDir( const std::string& dirUtf8 )
{
   try
   {
      std::error_code ec;
      std::filesystem::remove_all(
         std::filesystem::path( reinterpret_cast<const char8_t*>( dirUtf8.c_str() ) ), ec );
      // ec deliberately ignored: cleanup is best-effort (spec: item 5).
   }
   catch ( ... )
   {
      // Never let cleanup failure mask the run's real outcome.
   }
}

// RAII: delete the auto temp session dir on every exit path of run_blend.
struct AutoSessionDirGuard
{
   std::string dir;
   bool        active = false;
   ~AutoSessionDirGuard()
   {
      if ( active )
         RemoveSessionDir( dir );
   }
};

// Loads the worker-written seam/ownership map (<sessionDir>/seam_map.png,
// PROTOCOL.md section 6) and shows it as a new 8-bit "seam_map" image window.
// Called after a successful run, BEFORE AutoSessionDirGuard removes a temp
// session dir. Best-effort: a missing or unreadable file warns on the console
// but never fails the completed blend.
void ShowSeamMap( const std::string& sessionDirUtf8 )
{
   Console console;
   try
   {
      String path = IsoString( (sessionDirUtf8 + "/seam_map.png").c_str() ).UTF8ToUTF16();
      if ( !File::Exists( path ) )
      {
         console.WarningLn( "<end><cbr>** MegaMergeMosaic: seam map was requested but the worker "
                            "did not produce it: " + path );
         return;
      }

      FileFormat         format( ".png", true /*toRead*/, false /*toWrite*/ );
      FileFormatInstance file( format );
      ImageDescriptionArray images;
      if ( !file.Open( images, path ) || images.IsEmpty() )
         throw Error( "cannot open " + path );
      UInt8Image img;
      if ( !file.ReadImage( img ) )
         throw Error( "cannot read " + path );
      file.Close();

      ImageWindow window( img.Width(), img.Height(), img.NumberOfChannels(),
                          8 /*bitsPerSample*/, false /*floatSample*/,
                          img.NumberOfChannels() >= 3 /*color*/,
                          true /*initialProcessing*/, "seam_map" );
      View view = window.MainView();
      view.LockForWrite( false /*notify*/ );
      ImageVariant image = view.Image();
      static_cast<UInt8Image&>( *image ).Assign( img );
      view.UnlockForWrite( false /*notify*/ );
      window.Show();
   }
   catch ( ... )
   {
      // The mosaic itself completed; a broken diagnostic must not undo that.
      console.WarningLn( "<end><cbr>** MegaMergeMosaic: could not load the seam map image." );
   }
}

// Prints the worker's end-of-run mosaic summary (<sessionDir>/summary.json,
// PROTOCOL.md section 6) as a compact console block. Best-effort like
// ShowSeamMap: a missing or malformed file never fails the completed blend
// (an older worker simply doesn't write one).
void PrintSummary( const std::string& sessionDirUtf8 )
{
   Console console;
   try
   {
      String path = IsoString( (sessionDirUtf8 + "/summary.json").c_str() ).UTF8ToUTF16();
      if ( !File::Exists( path ) )
         return;
      IsoString text = File::ReadTextFile( path );
      json s = json::parse( text.c_str() );

      const uint64_t w  = s.at( "output" )[0].get<uint64_t>();
      const uint64_t h  = s.at( "output" )[1].get<uint64_t>();
      const uint64_t ch = s.at( "output" )[2].get<uint64_t>();
      const char* color = ( ch >= 3 ) ? "RGB" : "grayscale";

      console.WriteLn( "<end><cbr><br><b>Mosaic summary</b>" );
      console.WriteLn( String( IsoString().Format( "  panels          %zu",
         size_t( s.at( "panels" ).get<uint64_t>() ) ) ) );
      console.WriteLn( String( IsoString().Format( "  output          %llu x %llu px, %s (%.1f MP)",
         (unsigned long long)w, (unsigned long long)h, color,
         s.at( "megapixels" ).get<double>() ) ) );
      console.WriteLn( String( IsoString().Format( "  coverage        %.1f%% of the output area",
         100.0*s.at( "covered_frac" ).get<double>() ) ) );
      console.WriteLn( String( IsoString().Format( "  overlap edges   %zu, median overlap %.1f%% of panel",
         size_t( s.at( "overlap_edges" ).get<uint64_t>() ),
         100.0*s.at( "median_overlap_frac" ).get<double>() ) ) );
      console.WriteLn( String( IsoString().Format( "  seam step       median %.4g, max %.4g",
         s.at( "seam_delta_median" ).get<double>(),
         s.at( "seam_delta_max" ).get<double>() ) ) );
   }
   catch ( ... )
   {
      console.WarningLn( "<end><cbr>** MegaMergeMosaic: could not read the mosaic summary." );
   }
}

// Drives one fully-assembled job to completion and shows the output window on
// success. Throws on any fault or cancellation, leaving no window shown.
// `prog` is caller-owned (RunViews/RunFiles construct it before their probe
// phase, so the same abort state spans probe and run; the caller has already
// called Console().EnableAbort()).
void DriveHost( const std::string& worker_path, json init_body, const std::string& shm_name,
                const mmm::SlotLayout& layout, mmm::PanelSource& source, ConsoleProgress& prog )
{
   mmm::HostConfig cfg;
   cfg.worker_path = worker_path;
   cfg.layout      = layout;
   cfg.shm_name    = shm_name;
   cfg.init        = json{ { "Init", std::move( init_body ) } };

   ImageWindowCollector collector;

   mmm::Host host( std::move( cfg ), source, collector, &prog );
   prog.host = &host;
   s_activeHost.store( &host );

   try
   {
      host.run();
   }
   catch ( ... )
   {
      s_activeHost.store( nullptr );
      // Discard any partially-built (hidden, never-shown) output window so no
      // partial result lingers (spec section 9).
      if ( !collector.Window().IsNull() )
         collector.Window().Close();
      throw;
   }

   s_activeHost.store( nullptr );

   if ( host.cancelled() )
   {
      if ( !collector.Window().IsNull() )
         collector.Window().Close();
      // pcl::ProcessAborted (not pcl::Error, not std::exception -- see
      // MmmProcess.cpp ExecuteGlobal) signals a clean user cancellation: PixInsight
      // treats it as an abort rather than a fault, so no red error dialog appears.
      throw ProcessAborted();
   }

   // A run that completed (Done) without ever streaming a Begin/band leaves no
   // window -- surface that as a clean error rather than Show()ing a null window.
   if ( collector.Window().IsNull() )
      throw Error( "MegaMergeMosaic: the worker completed without producing an output image." );
   collector.Window().Show();
}

// ---- Views path (Aligned / Solved) ----------------------------------------

void RunViews( const Params& in, const std::string& worker_path )
{
   // Progress/abort observer shared by the probe phase and the run itself
   // (want_cancel is sticky across both).
   Console().EnableAbort();
   ConsoleProgress prog;

   // Resolve every stored view id to a live view.
   Array<View> views;
   for ( const String& id : in.viewIds )
   {
      View v = View::ViewById( id );
      if ( v.IsNull() )
         throw Error( "MegaMergeMosaic: input view not found: " + id );
      views.Add( v );
   }

   // Lock all views for the duration of the run (ViewPanelSource contract);
   // unlock on every exit path. The lock loop is INSIDE the try so that a
   // throwing Lock() still releases the [0, locked) views already acquired
   // (locked counts successfully-locked views: it is only incremented after a
   // Lock() returns).
   size_type locked = 0;
   try
   {
      for ( ; locked < views.Length(); ++locked )
         views[locked].Lock();

      // Collect geometry; note whether all views share identical dimensions.
      Array<uint64_t> ws, hs, cs;
      bool uniform = true;
      for ( size_type i = 0; i < views.Length(); ++i )
      {
         const ImageVariant img = views[i].Image();
         if ( !img )
            throw Error( "MegaMergeMosaic: input view carries no image: " + views[i].FullId() );
         if ( img.IsComplexSample() )
            throw Error( "MegaMergeMosaic: complex images are not supported: " + views[i].FullId() );
         const uint64_t w = uint64_t( img.Width() );
         const uint64_t h = uint64_t( img.Height() );
         const uint64_t c = uint64_t( img.NumberOfChannels() );
         ws.Add( w );
         hs.Add( h );
         cs.Add( c );
         if ( i > 0 && ( w != ws[0] || h != hs[0] || c != cs[0] ) )
            uniform = false;
      }

      // Resolve the effective JobMode (spec section 10.1): the override wins;
      // Auto classifies by uniform-vs-differing geometry.
      bool solved;
      switch ( in.inputSelect )
      {
      case MmmInputSelectParameter::Aligned: solved = false;      break;
      case MmmInputSelectParameter::Solved:  solved = true;       break;
      default:                               solved = !uniform;   break;
      }

      // Solved mode requires each view to carry an astrometric solution.
      if ( solved )
         for ( size_type i = 0; i < views.Length(); ++i )
            if ( !views[i].Window().HasAstrometricSolution() )
               throw Error( "MegaMergeMosaic: solved mode requires an astrometric solution on every "
                            "view; this one has none: " + views[i].FullId() );

      const uint64_t ch        = cs[0];
      const uint64_t band_rows = uint64_t( uint32_t( in.bandRows ) );

      // Ordered PanelDescs (PROTOCOL.md section 6). Solved mode attaches the
      // plate solution verbatim as each panel's properties.
      json panels = json::array();
      uint64_t max_panel_w = 0;
      for ( size_type i = 0; i < views.Length(); ++i )
      {
         json pd;
         pd["panel_id"]   = uint32_t( i );
         pd["width"]      = ws[i];
         pd["height"]     = hs[i];
         pd["channels"]   = cs[i];
         pd["properties"] = solved ? extract_astrometry_props( views[i] ) : json::array();
         panels.push_back( std::move( pd ) );
         if ( ws[i] > max_panel_w )
            max_panel_w = ws[i];
      }

      // Init body sans slot sizing (filled below).
      json init_body;
      // protocol_version + worker_version are stamped by mmm::Host (run and probes).
      init_body["shm_name"]         = "";
      init_body["slot_bytes"]       = 0;
      init_body["input_slots"]      = kInputSlots;
      init_body["output_slots"]     = kOutputSlots;
      init_body["panels"]           = panels;
      init_body["session_dir"]      = in.sessionDir;
      init_body["params"]           = BuildParams( in );

      uint64_t slot_bytes = 0;
      if ( solved )
      {
         // Aligned mode knows the canvas; solved mode does not -- probe the
         // worker for the reprojected frame width so slots fit BOTH an input
         // band (a raw panel's width) and an output band (the frame's width).
         init_body["mode"]   = "Solved";
         init_body["canvas"] = { uint64_t( 0 ), uint64_t( 0 ), ch };

         uint64_t fw = 0, fh = 0, fch = 0;
         try
         {
            mmm::Host::probe_frame( worker_path, init_body, fw, fh, fch, &prog );
         }
         catch ( const mmm::HostCancelled& )
         {
            throw ProcessAborted();
         }
         const uint64_t width = ( fw > max_panel_w ) ? fw : max_panel_w;
         slot_bytes = width * ch * band_rows * 4;
      }
      else
      {
         if ( !uniform )
            throw Error( "MegaMergeMosaic: aligned mode requires all views to share the same "
                         "dimensions; use solved mode for unregistered panels." );
         init_body["mode"]   = "Aligned";
         init_body["canvas"] = { ws[0], hs[0], ch };
         slot_bytes = ws[0] * ch * band_rows * 4;
      }

      init_body["slot_bytes"] = slot_bytes;
      const std::string shm_name = MakeShmName();
      init_body["shm_name"]   = shm_name;

      mmm::SlotLayout layout{ slot_bytes, kInputSlots, kOutputSlots };
      ViewPanelSource source( views );
      DriveHost( worker_path, std::move( init_body ), shm_name, layout, source, prog );
   }
   catch ( ... )
   {
      for ( size_type i = 0; i < locked; ++i )
         views[i].Unlock();
      throw;
   }

   for ( size_type i = 0; i < locked; ++i )
      views[i].Unlock();
}

// ---- Files path ------------------------------------------------------------

void RunFiles( const Params& in, const std::string& worker_path )
{
   // Progress/abort observer shared by the probe phase and the run itself
   // (want_cancel is sticky across both).
   Console console;
   console.EnableAbort();
   ConsoleProgress prog;

   // Delegate the metadata pass to the worker (PROTOCOL.md section 11,
   // --probe-panels): header-only parallel reads in a child process while this
   // GUI thread pumps events via prog.on_idle() -- the module never opens a
   // panel file itself, so a run over dozens of multi-GB panels cannot freeze
   // the UI, and the Console abort works even during the scan. The reply also
   // carries the solved mosaic frame (the worker's own choose_frame) exactly
   // when the run can resolve to solved mode, replacing the separate
   // host-side property read + --probe-frame round trip.
   std::vector<std::string> paths_utf8;
   paths_utf8.reserve( size_t( in.filePaths.Length() ) );
   for ( const String& path : in.filePaths )
      paths_utf8.push_back( std::string( path.ToUTF8().c_str() ) );

   console.Write( String().Format( "<end><cbr>Reading metadata of %u panel files... ",
                                   unsigned( paths_utf8.size() ) ) );
   mmm::PanelProbeResult probe;
   try
   {
      probe = mmm::Host::probe_panels( worker_path, paths_utf8,
                                       InputSelectWireString( in.inputSelect ), &prog );
   }
   catch ( const mmm::HostCancelled& )
   {
      throw ProcessAborted();
   }
   console.WriteLn( "done." );

   json     panels     = json::array();   // the run panels (empty properties)
   json     file_paths = json::array();
   uint64_t max_w      = 0;
   for ( size_t i = 0; i < probe.panels.size(); ++i )
   {
      const mmm::ProbedPanel& p = probe.panels[i];
      json pd;
      pd["panel_id"]   = uint32_t( i );
      pd["width"]      = p.width;
      pd["height"]     = p.height;
      pd["channels"]   = p.channels;
      pd["properties"] = json::array();
      panels.push_back( std::move( pd ) );
      file_paths.push_back( paths_utf8[i] );
      if ( p.width > max_w )
         max_w = p.width;
   }
   const uint64_t ch0       = probe.panels[0].channels;
   const uint64_t band_rows = uint64_t( uint32_t( in.bandRows ) );

   // Output-slot sizing. The output band width is the width of the frame the
   // worker computes, which differs by how Files mode resolves aligned-vs-solved
   // (PROTOCOL.md section 11):
   //   * SOLVED  -- the worker reprojects onto its own choose_frame, whose width
   //     can EXCEED every input file's width. Sizing by input width alone would
   //     silently overrun the output slot (section 7 hazard). The probe reports
   //     that frame (has_frame) exactly when the run can resolve to solved
   //     (input_select Solved or Auto, and every file carries a plate
   //     solution); take the max.
   //   * ALIGNED -- the output canvas width == the (shared) file width, so the
   //     widest input file is exact. This covers input_select Aligned, and Auto
   //     with registered (no-solution) files -- the probe reports no frame.
   uint64_t width = max_w;
   if ( probe.has_frame && probe.frame_w > width )
      width = probe.frame_w;
   const uint64_t slot_bytes = width * ch0 * band_rows * 4;

   json init_body;
   // protocol_version + worker_version are stamped by mmm::Host (run and probes).
   init_body["slot_bytes"]       = slot_bytes;
   init_body["input_slots"]      = kInputSlots;
   init_body["output_slots"]     = kOutputSlots;
   init_body["canvas"]           = { uint64_t( 0 ), uint64_t( 0 ), ch0 };
   init_body["panels"]           = panels;
   init_body["mode"]             = json{ { "Files",
                                           { { "paths", file_paths },
                                             { "input_select", InputSelectWireString( in.inputSelect ) } } } };
   init_body["session_dir"]      = in.sessionDir;
   init_body["params"]           = BuildParams( in );

   const std::string shm_name = MakeShmName();
   init_body["shm_name"]      = shm_name;

   mmm::SlotLayout layout{ slot_bytes, kInputSlots, kOutputSlots };
   NullPanelSource source;
   DriveHost( worker_path, std::move( init_body ), shm_name, layout, source, prog );
}

} // namespace

// ----------------------------------------------------------------------------

void run_blend( MmmBlendInstance& in )
{
   WriteBanner();

   // 1. Validate: non-empty and a single input type (spec section 10.1).
   const bool haveViews = !in.p_viewIds.IsEmpty();
   const bool haveFiles = !in.p_filePaths.IsEmpty();
   if ( !haveViews && !haveFiles )
      throw Error( "MegaMergeMosaic: no input selected. Add at least two views or files." );
   if ( haveViews && haveFiles )
      throw Error( "MegaMergeMosaic: mixed input. Select either views or files, not both." );
   if ( ( haveViews && in.p_viewIds.Length() < 2 ) || ( haveFiles && in.p_filePaths.Length() < 2 ) )
      throw Error( "MegaMergeMosaic: need at least two views or files to blend." );

   const std::string worker_path = ResolveWorkerPath();

   // Snapshot the private parameters (run_blend is the sole friend) so the
   // internal helpers need no access of their own.
   Params p;
   p.viewIds        = in.p_viewIds;
   p.filePaths      = in.p_filePaths;
   p.inputSelect    = in.p_inputSelect;
   p.sessionDir     = std::string( in.p_sessionDir.ToUTF8().c_str() );
   p.feather        = in.p_feather;
   p.blendMode      = in.p_blendMode;
   p.flatten        = in.p_flatten;
   p.flattenEnabled = in.p_flattenEnabled;
   p.defectVeto     = in.p_defectVeto;
   p.seamMap        = in.p_seamMap;
   p.surfaceOrder   = in.p_surfaceOrder;
   p.gainMode       = in.p_gainMode;
   p.bandRows       = in.p_bandRows;

   // Empty session dir (the default): run out of a fresh directory under the
   // system temp dir and remove it afterwards, success or failure. A
   // user-specified directory is used as-is and preserved (its cache makes
   // re-runs with the same inputs resume completed analysis stages).
   AutoSessionDirGuard sessionGuard;
   if ( p.sessionDir.empty() )
   {
      String t = File::SystemTempDirectory();
      if ( !t.EndsWith( '/' ) )
         t += '/';
      t += "mmm-session-" +
           String( (unsigned long)
#ifdef _WIN32
                   ::GetCurrentProcessId()
#else
                   ::getpid()
#endif
                 ) + "-" + String( (unsigned long long) s_runCounter.fetch_add( 1 ) );
      p.sessionDir      = std::string( t.ToUTF8().c_str() );
      // Defend against a stale same-named dir left behind by a failed prior
      // cleanup (e.g. after a module reload) being silently reused as cache.
      RemoveSessionDir( p.sessionDir );
      sessionGuard.dir    = p.sessionDir;
      sessionGuard.active = true;
   }

   if ( haveViews )
      RunViews( p, worker_path );
   else
      RunFiles( p, worker_path );

   // The run succeeded; surface the requested seam map and the end-of-run
   // summary while the session dir (and the files the worker wrote into it)
   // still exists -- sessionGuard removes a temp dir when this function
   // returns.
   if ( p.seamMap )
      ShowSeamMap( p.sessionDir );
   PrintSummary( p.sessionDir );
}

// ----------------------------------------------------------------------------

} // namespace pcl
