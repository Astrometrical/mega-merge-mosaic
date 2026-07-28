// MmmExecution.cpp -- see MmmExecution.h. Spec section 10.1 execution flow.

#include "MmmExecution.h"

#include <dlfcn.h>
#include <unistd.h>

#include <atomic>
#include <cmath>
#include <string>
#include <vector>

#include <pcl/Array.h>
#include <pcl/Console.h>
#include <pcl/Exception.h>
#include <pcl/File.h>
#include <pcl/FileFormat.h>
#include <pcl/FileFormatInstance.h>
#include <pcl/ImageDescription.h>
#include <pcl/ImageVariant.h>
#include <pcl/ImageWindow.h>
#include <pcl/MetaModule.h>
#include <pcl/Property.h>
#include <pcl/View.h>

#include "AstrometryProps.h"
#include "ImageWindowCollector.h"
#include "MmmInterface.h"
#include "MmmParameters.h"
#include "MmmProcess.h"
#include "ViewPanelSource.h"
#include "mmm_host.h"
#include "third_party/json.hpp"

namespace pcl
{

using nlohmann::json;

// ----------------------------------------------------------------------------
// Module-scope state for cooperative cancellation. run_blend() publishes the
// live host here for the duration of a run so request_cancel() (interface
// Cancel button / Console abort) can reach it; both Host::cancel() and the
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
   float         feather;
   pcl_enum      blendMode;
   int32         flatten;
   bool          flattenEnabled;
   int32         roi[ 4 ];
   bool          roiEnabled;
   int32         downsample;
   bool          defectVeto;
   int32         surfaceOrder;
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
// thread between worker frames, so it is safe to touch the Console and the
// interface here. We echo a percentage to the Console, mirror it to the
// interface's progress Label, pump the GUI event queue so a queued Cancel-
// button click is delivered, and poll the Console's own Abort button; either
// path calls Host::cancel(). This is the synchronous v1 (spec section 15): the
// tool window is not repainted mid-run beyond ProcessEvents() pumps.
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
class ConsoleProgress : public mmm::ProgressCallback
{
public:
   mmm::Host* host = nullptr;

   void on_progress( const std::string& stage, uint64_t done, uint64_t total ) override
   {
      std::string msg = stage;
      msg += ": ";
      if ( total > 0 )
         msg += std::to_string( 100 * done / total ) + "%";
      else
         msg += std::to_string( done );

      String s( msg.c_str() );
      m_console.WriteLn( s );
      if ( TheMmmBlendInterface != nullptr )
         TheMmmBlendInterface->SetProgressText( s );

      // Deliver any pending interface events (e.g. a Cancel click) and surface
      // a Console abort request; both drive the same cancellation path.
      Module->ProcessEvents();
      if ( m_console.AbortRequested() )
      {
         m_console.Abort();
         if ( host != nullptr )
            host->cancel();
      }
   }

private:
   Console m_console;
};

// A PanelSource the worker never calls: Files mode reads panels off disk itself
// (PROTOCOL.md section 11), so no BandRequest is ever issued.
struct NullPanelSource : mmm::PanelSource
{
   bool fill_band( uint32_t, uint64_t, uint64_t, float* ) override { return false; }
};

// ----------------------------------------------------------------------------

// Directory containing this loaded module .so, via dladdr on a module symbol
// (PCL exposes no module-path API; PCL_API_REFERENCE.md section 8).
String ModuleDirectory()
{
   Dl_info info;
   if ( dladdr( reinterpret_cast<void*>( &run_blend ), &info ) != 0 && info.dli_fname != nullptr )
   {
      String path( info.dli_fname );
      return File::ExtractDrive( path ) + File::ExtractDirectory( path );
   }
   throw Error( "MosaicMerge: could not determine the module path via dladdr()." );
}

// Absolute path to the sibling mmm-ipc-worker binary; errors clearly if absent.
std::string ResolveWorkerPath()
{
   String dir = ModuleDirectory();
   if ( !dir.EndsWith( '/' ) )
      dir += '/';
   String worker = dir + "mmm-ipc-worker";
   if ( !File::Exists( worker ) )
      throw Error( "MosaicMerge: worker binary not found next to the module: " + worker +
                   "\nBuild it with `cargo build --release -p mmm-ipc-worker` and copy "
                   "`mmm-ipc-worker` beside mmm-pxm.so." );
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

// Builds the BlendParamsWire object (PROTOCOL.md section 6) from the parameters.
// Enforces the finite-float precondition on feather_px (the only float here).
json BuildParams( const Params& in )
{
   if ( !std::isfinite( in.feather ) )
      throw Error( "MosaicMerge: feather value is not finite." );

   json p;
   p["feather_px"]    = double( in.feather );
   p["downsample"]    = uint32_t( in.downsample );
   p["band_rows"]     = uint32_t( in.bandRows );
   p["mode"]          = BlendModeWireString( in.blendMode );
   if ( in.roiEnabled )
      p["roi"] = json::array( { uint64_t( uint32_t( in.roi[0] ) ), uint64_t( uint32_t( in.roi[1] ) ),
                                uint64_t( uint32_t( in.roi[2] ) ), uint64_t( uint32_t( in.roi[3] ) ) } );
   else
      p["roi"] = nullptr;
   p["defect_veto"]   = bool( in.defectVeto );
   if ( in.flattenEnabled )
      p["flatten"] = uint32_t( in.flatten );
   else
      p["flatten"] = nullptr;
   p["surface_order"] = uint32_t( in.surfaceOrder );
   return p;
}

// A fresh, process-unique shm segment name (PROTOCOL.md section 2).
std::string MakeShmName()
{
   return "/mmm-pxm-" + std::to_string( (unsigned long) ::getpid() ) + "-" +
          std::to_string( s_runCounter.fetch_add( 1 ) );
}

// Enable/disable the interface Cancel button around a run (best-effort; the
// interface may not be open).
void SetRunningUI( bool running )
{
   if ( TheMmmBlendInterface != nullptr )
      TheMmmBlendInterface->SetBlendRunning( running );
}

// Drives one fully-assembled job to completion and shows the output window on
// success. Throws on any fault or cancellation, leaving no window shown.
void DriveHost( const std::string& worker_path, json init_body, const std::string& shm_name,
                const mmm::SlotLayout& layout, mmm::PanelSource& source )
{
   mmm::HostConfig cfg;
   cfg.worker_path = worker_path;
   cfg.layout      = layout;
   cfg.shm_name    = shm_name;
   cfg.init        = json{ { "Init", std::move( init_body ) } };

   ImageWindowCollector collector;
   ConsoleProgress      prog;

   Console().EnableAbort();
   mmm::Host host( std::move( cfg ), source, collector, &prog );
   prog.host = &host;
   s_activeHost.store( &host );
   SetRunningUI( true );

   try
   {
      host.run();
   }
   catch ( ... )
   {
      s_activeHost.store( nullptr );
      SetRunningUI( false );
      // Discard any partially-built (hidden, never-shown) output window so no
      // partial result lingers (spec section 9).
      if ( !collector.Window().IsNull() )
         collector.Window().Close();
      throw;
   }

   s_activeHost.store( nullptr );
   SetRunningUI( false );

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
      throw Error( "MosaicMerge: the worker completed without producing an output image." );
   collector.Window().Show();
}

// ---- Views path (Aligned / Solved) ----------------------------------------

void RunViews( const Params& in, const std::string& worker_path )
{
   // Resolve every stored view id to a live view.
   Array<View> views;
   for ( const String& id : in.viewIds )
   {
      View v = View::ViewById( id );
      if ( v.IsNull() )
         throw Error( "MosaicMerge: input view not found: " + id );
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
            throw Error( "MosaicMerge: input view carries no image: " + views[i].FullId() );
         if ( img.IsComplexSample() )
            throw Error( "MosaicMerge: complex images are not supported: " + views[i].FullId() );
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
               throw Error( "MosaicMerge: solved mode requires an astrometric solution on every "
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
      init_body["protocol_version"] = 2;
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
         mmm::Host::probe_frame( worker_path, init_body, fw, fh, fch );
         const uint64_t width = ( fw > max_panel_w ) ? fw : max_panel_w;
         slot_bytes = width * ch * band_rows * 4;
      }
      else
      {
         if ( !uniform )
            throw Error( "MosaicMerge: aligned mode requires all views to share the same "
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
      DriveHost( worker_path, std::move( init_body ), shm_name, layout, source );
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
   // Read each file's geometry AND astrometric properties (no pixel load). The
   // run itself sends empty PanelDesc.properties -- the worker re-derives the
   // solution from the files it reads (PROTOCOL.md section 11) -- but we keep the
   // read properties to size the output slot safely (see the probe below).
   json     panels        = json::array();   // the run panels (empty properties)
   json     probe_panels  = json::array();   // the same panels WITH properties, for --probe-frame
   json     file_paths    = json::array();
   uint64_t max_w         = 0;
   uint64_t ch0           = 0;
   uint32_t pid           = 0;
   bool     allHaveSolution = true;
   for ( const String& path : in.filePaths )
   {
      FileFormat         format( File::ExtractExtension( path ), true /*toRead*/, false /*toWrite*/ );
      FileFormatInstance file( format );
      ImageDescriptionArray images;
      if ( !file.Open( images, path ) || images.IsEmpty() )
         throw Error( "MosaicMerge: cannot read image file: " + path );
      const ImageInfo& info = images[0].info;

      // Read this file's astrometric solution, if the format carries one.
      json props = json::array();
      if ( format.CanStoreImageProperties() )
         props = extract_astrometry_props( file.ReadImageProperties() );
      if ( props.empty() )
         allHaveSolution = false;

      json pd;
      pd["panel_id"] = pid;
      pd["width"]    = uint64_t( info.width );
      pd["height"]   = uint64_t( info.height );
      pd["channels"] = uint64_t( info.numberOfChannels );

      json ppd            = pd;
      pd["properties"]    = json::array();
      ppd["properties"]   = std::move( props );
      panels.push_back( std::move( pd ) );
      probe_panels.push_back( std::move( ppd ) );
      file_paths.push_back( std::string( path.ToUTF8().c_str() ) );

      if ( uint64_t( info.width ) > max_w )
         max_w = uint64_t( info.width );
      if ( pid == 0 )
         ch0 = uint64_t( info.numberOfChannels );

      file.Close();
      ++pid;
   }

   const uint64_t band_rows = uint64_t( uint32_t( in.bandRows ) );

   // Output-slot sizing. The output band width is the width of the frame the
   // worker computes, which differs by how Files mode resolves aligned-vs-solved
   // (PROTOCOL.md section 11):
   //   * SOLVED  -- the worker reprojects onto its own choose_frame, whose width
   //     can EXCEED every input file's width. Sizing by input width alone would
   //     silently overrun the output slot (section 7 hazard). So when the run can
   //     resolve to solved (input_select Solved or Auto) AND every file carries a
   //     plate solution, probe the worker for the frame width -- exactly as
   //     test_golden_solved.cpp sizes its Files(Solved) run -- and take the max.
   //   * ALIGNED -- the output canvas width == the (shared) file width, so the
   //     widest input file is exact. This covers input_select Aligned, and Auto
   //     with registered (no-solution) files.
   const bool canSolve = ( in.inputSelect == MmmInputSelectParameter::Solved ) ||
                         ( in.inputSelect == MmmInputSelectParameter::Auto );
   uint64_t width = max_w;
   if ( canSolve && allHaveSolution )
   {
      json probe_init;
      probe_init["protocol_version"] = 2;
      probe_init["shm_name"]         = "";
      probe_init["slot_bytes"]       = 0;
      probe_init["input_slots"]      = 0;
      probe_init["output_slots"]     = 0;
      probe_init["canvas"]           = { uint64_t( 0 ), uint64_t( 0 ), ch0 };
      probe_init["panels"]           = std::move( probe_panels );
      probe_init["mode"]             = "Solved";
      probe_init["session_dir"]      = "";
      probe_init["params"]           = BuildParams( in );

      uint64_t fw = 0, fh = 0, fch = 0;
      mmm::Host::probe_frame( worker_path, probe_init, fw, fh, fch );
      if ( fw > width )
         width = fw;
   }
   const uint64_t slot_bytes = width * ch0 * band_rows * 4;

   json init_body;
   init_body["protocol_version"] = 2;
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
   DriveHost( worker_path, std::move( init_body ), shm_name, layout, source );
}

} // namespace

// ----------------------------------------------------------------------------

void run_blend( MmmBlendInstance& in )
{
   // 1. Validate: non-empty and a single input type (spec section 10.1).
   const bool haveViews = !in.p_viewIds.IsEmpty();
   const bool haveFiles = !in.p_filePaths.IsEmpty();
   if ( !haveViews && !haveFiles )
      throw Error( "MosaicMerge: no input selected. Add at least two views or files." );
   if ( haveViews && haveFiles )
      throw Error( "MosaicMerge: mixed input. Select either views or files, not both." );
   if ( ( haveViews && in.p_viewIds.Length() < 2 ) || ( haveFiles && in.p_filePaths.Length() < 2 ) )
      throw Error( "MosaicMerge: need at least two views or files to blend." );
   if ( in.p_sessionDir.IsEmpty() )
      throw Error( "MosaicMerge: select a session directory (the worker caches analysis there)." );

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
   p.roi[0]         = in.p_roi[0];
   p.roi[1]         = in.p_roi[1];
   p.roi[2]         = in.p_roi[2];
   p.roi[3]         = in.p_roi[3];
   p.roiEnabled     = in.p_roiEnabled;
   p.downsample     = in.p_downsample;
   p.defectVeto     = in.p_defectVeto;
   p.surfaceOrder   = in.p_surfaceOrder;
   p.bandRows       = in.p_bandRows;

   if ( haveViews )
      RunViews( p, worker_path );
   else
      RunFiles( p, worker_path );
}

void request_cancel()
{
   if ( mmm::Host* h = s_activeHost.load() )
      h->cancel();
}

// ----------------------------------------------------------------------------

} // namespace pcl
