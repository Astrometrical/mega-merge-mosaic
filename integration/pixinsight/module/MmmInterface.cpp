// MmmInterface.cpp -- implementation of the trivial MergeMosaic interface.

#include "MmmInterface.h"
#include "MmmProcess.h"

namespace pcl
{

// ----------------------------------------------------------------------------

MmmBlendInterface* TheMmmBlendInterface = nullptr;

// ----------------------------------------------------------------------------

MmmBlendInterface::MmmBlendInterface()
{
   // Constructing a ProcessInterface self-registers it under pcl::Module.
   TheMmmBlendInterface = this;
}

IsoString MmmBlendInterface::Id() const
{
   return "MosaicMerge";
}

MetaProcess* MmmBlendInterface::Process() const
{
   return TheMmmBlendProcess;
}

InterfaceFeatures MmmBlendInterface::Features() const
{
   // Static interface that executes in the global context.
   return InterfaceFeature::DefaultGlobal;
}

ProcessImplementation* MmmBlendInterface::NewProcess() const
{
   return new MmmBlendInstance( TheMmmBlendProcess );
}

bool MmmBlendInterface::Launch( const MetaProcess&, const ProcessImplementation*,
                                bool& dynamic, unsigned& /*flags*/ )
{
   // Task 1: no child controls are built yet. The interface is itself an
   // (empty) Control; a static interface simply reports success.
   dynamic = false;
   return true;
}

// ----------------------------------------------------------------------------

} // namespace pcl
