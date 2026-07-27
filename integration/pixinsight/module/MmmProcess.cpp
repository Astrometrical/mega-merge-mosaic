// MmmProcess.cpp -- implementation of the trivial MergeMosaic process/instance.

#include "MmmProcess.h"
#include "MmmInterface.h"

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
{
}

MmmBlendInstance::MmmBlendInstance( const MmmBlendInstance& x )
   : ProcessImplementation( x )
{
   Assign( x );
}

void MmmBlendInstance::Assign( const ProcessImplementation& p )
{
   // No parameters yet; nothing to copy. The dynamic_cast documents intent and
   // guards against cross-type assignment once parameters exist.
   const MmmBlendInstance* x = dynamic_cast<const MmmBlendInstance*>( &p );
   if ( x != nullptr )
   {
      // (future: copy parameter members from *x)
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
   // Task 1: no-op. Real blend orchestration arrives in later tasks.
   return true;
}

// ----------------------------------------------------------------------------

} // namespace pcl
