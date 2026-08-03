// MmmProcess.cpp -- implementation of the trivial MergeMosaic process/instance.

#include "MmmProcess.h"
#include "MmmExecution.h"
#include "MmmInterface.h"
#include "MmmParameters.h"

#include <exception>

#include <pcl/Exception.h>

namespace pcl
{

// ----------------------------------------------------------------------------

MmmBlendProcess* TheMmmBlendProcess = nullptr;

// ----------------------------------------------------------------------------

MmmBlendProcess::MmmBlendProcess()
{
   // Constructing a MetaProcess self-registers it under pcl::Module.
   TheMmmBlendProcess = this;
}

IsoString MmmBlendProcess::Id() const
{
   return "MosaicMerge";
}

IsoString MmmBlendProcess::Categories() const
{
   return "Mosaic";
}

uint32 MmmBlendProcess::Version() const
{
   return 0x100;   // 1.0.0
}

String MmmBlendProcess::Description() const
{
   return "Fast merge/blend for pre-aligned astro mosaic panels.";
}

ProcessImplementation* MmmBlendProcess::Create() const
{
   return new MmmBlendInstance( this );
}

ProcessImplementation* MmmBlendProcess::Clone( const ProcessImplementation& p ) const
{
   const MmmBlendInstance* instance = dynamic_cast<const MmmBlendInstance*>( &p );
   return (instance != nullptr) ? new MmmBlendInstance( *instance ) : nullptr;
}

bool MmmBlendProcess::CanProcessViews() const
{
   return false;   // global-only process
}

bool MmmBlendProcess::PrefersGlobalExecution() const
{
   return true;
}

ProcessInterface* MmmBlendProcess::DefaultInterface() const
{
   return TheMmmBlendInterface;
}

// ----------------------------------------------------------------------------

MmmBlendInstance::MmmBlendInstance( const MetaProcess* m )
   : ProcessImplementation( m )
   , p_inputSelect( TheMmmInputSelectParameter->ElementValue( TheMmmInputSelectParameter->DefaultValueIndex() ) )
   , p_feather( int32( TheMmmFeatherParameter->DefaultValue() ) )
   , p_blendMode( TheMmmBlendModeParameter->ElementValue( TheMmmBlendModeParameter->DefaultValueIndex() ) )
   , p_flatten( int32( TheMmmFlattenParameter->DefaultValue() ) )
   , p_flattenEnabled( TheMmmFlattenEnabledParameter->DefaultValue() )
   , p_defectVeto( TheMmmDefectVetoParameter->DefaultValue() )
   , p_surfaceOrder( int32( TheMmmSurfaceOrderParameter->DefaultValue() ) )
   , p_bandRows( int32( TheMmmBandRowsParameter->DefaultValue() ) )
{
}

MmmBlendInstance::MmmBlendInstance( const MmmBlendInstance& x )
   : ProcessImplementation( x )
{
   Assign( x );
}

void MmmBlendInstance::Assign( const ProcessImplementation& p )
{
   // Deep-copy every parameter member. The dynamic_cast guards against a
   // cross-type assignment; a no-op on a foreign type is the correct behavior.
   const MmmBlendInstance* x = dynamic_cast<const MmmBlendInstance*>( &p );
   if ( x != nullptr )
   {
      p_viewIds       = x->p_viewIds;
      p_filePaths     = x->p_filePaths;
      p_inputSelect   = x->p_inputSelect;
      p_sessionDir    = x->p_sessionDir;
      p_feather       = x->p_feather;
      p_blendMode     = x->p_blendMode;
      p_flatten       = x->p_flatten;
      p_flattenEnabled = x->p_flattenEnabled;
      p_defectVeto    = x->p_defectVeto;
      p_surfaceOrder  = x->p_surfaceOrder;
      p_bandRows      = x->p_bandRows;
   }
}

bool MmmBlendInstance::CanExecuteGlobal( String& whyNot ) const
{
   whyNot.Clear();
   return true;
}

bool MmmBlendInstance::CanExecuteOn( const View&, String& whyNot ) const
{
   whyNot = "MosaicMerge can only be executed in the global context.";
   return false;
}

bool MmmBlendInstance::ExecuteGlobal()
{
   // Drive the whole blend (spec section 10.1). A pcl::Error/pcl::ProcessAborted
   // propagates so PixInsight surfaces it (and no output window is shown); a
   // host-library failure (mmm::HostError, a std::exception) is converted to a
   // clean pcl::Error. On any error the source views are left intact and no
   // partial window exists (fault isolation, spec section 9).
   try
   {
      run_blend( *this );
   }
   catch ( const Error& )
   {
      throw;
   }
   catch ( const std::exception& e )
   {
      throw Error( String( e.what() ) );
   }
   return true;
}

// ----------------------------------------------------------------------------
// Parameter storage hooks. The core calls these keyed by the MetaParameter*
// singleton pointer (identity dispatch) plus a tableRow index for table/column
// parameters. For the two MetaTables the core never Lock()s the table itself:
// row count is served by ParameterLength and row allocation by
// AllocateParameter; only the MetaString columns and the scalar parameters are
// ever locked for a value pointer.
// ----------------------------------------------------------------------------

void* MmmBlendInstance::LockParameter( const MetaParameter* p, size_type tableRow )
{
   // Table string columns: return a raw pointer into that row's UTF-16 buffer.
   if ( p == TheMmmViewIdParameter )
      return p_viewIds[tableRow].Begin();
   if ( p == TheMmmPathParameter )
      return p_filePaths[tableRow].Begin();

   // Scalars.
   if ( p == TheMmmInputSelectParameter )   return &p_inputSelect;
   if ( p == TheMmmSessionDirParameter )    return p_sessionDir.Begin();
   if ( p == TheMmmFeatherParameter )       return &p_feather;
   if ( p == TheMmmBlendModeParameter )     return &p_blendMode;
   if ( p == TheMmmFlattenParameter )       return &p_flatten;
   if ( p == TheMmmFlattenEnabledParameter ) return &p_flattenEnabled;
   if ( p == TheMmmDefectVetoParameter )    return &p_defectVeto;
   if ( p == TheMmmSurfaceOrderParameter )  return &p_surfaceOrder;
   if ( p == TheMmmBandRowsParameter )      return &p_bandRows;

   return nullptr;
}

bool MmmBlendInstance::AllocateParameter( size_type sizeOrLength, const MetaParameter* p, size_type tableRow )
{
   // Tables: sizeOrLength is a ROW COUNT -> resize the backing Array<String>.
   if ( p == TheMmmInputImagesParameter )
   {
      p_viewIds.Clear();
      if ( sizeOrLength > 0 )
         p_viewIds.Add( String(), sizeOrLength );
      return true;
   }
   if ( p == TheMmmFilePathsParameter )
   {
      p_filePaths.Clear();
      if ( sizeOrLength > 0 )
         p_filePaths.Add( String(), sizeOrLength );
      return true;
   }

   // String columns / scalar strings: sizeOrLength is a CHARACTER LENGTH.
   if ( p == TheMmmViewIdParameter )
   {
      p_viewIds[tableRow].Clear();
      if ( sizeOrLength > 0 )
         p_viewIds[tableRow].SetLength( sizeOrLength );
      return true;
   }
   if ( p == TheMmmPathParameter )
   {
      p_filePaths[tableRow].Clear();
      if ( sizeOrLength > 0 )
         p_filePaths[tableRow].SetLength( sizeOrLength );
      return true;
   }
   if ( p == TheMmmSessionDirParameter )
   {
      p_sessionDir.Clear();
      if ( sizeOrLength > 0 )
         p_sessionDir.SetLength( sizeOrLength );
      return true;
   }

   return false;
}

size_type MmmBlendInstance::ParameterLength( const MetaParameter* p, size_type tableRow ) const
{
   // Tables: report the ROW COUNT.
   if ( p == TheMmmInputImagesParameter )
      return p_viewIds.Length();
   if ( p == TheMmmFilePathsParameter )
      return p_filePaths.Length();

   // String columns / scalar strings: report the CHARACTER LENGTH.
   if ( p == TheMmmViewIdParameter )
      return p_viewIds[tableRow].Length();
   if ( p == TheMmmPathParameter )
      return p_filePaths[tableRow].Length();
   if ( p == TheMmmSessionDirParameter )
      return p_sessionDir.Length();

   return 0;
}

// ----------------------------------------------------------------------------

} // namespace pcl
