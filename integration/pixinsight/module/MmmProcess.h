// MmmProcess.h -- MergeMosaic global-context process + instance (Task 1: trivial skeleton).
//
// MmmBlendProcess is a pure global-context MetaProcess (no target view); its
// instance, MmmBlendInstance, currently executes as a no-op (ExecuteGlobal()
// returns true). Real enumeration/blend logic arrives in later tasks.
//
// Signatures follow PCL_API_REFERENCE.md section 2, verified against
// /opt/PixInsight/include/pcl/{MetaProcess,ProcessImplementation}.h. Several of
// these methods have default bodies that deliberately throw at runtime if not
// overridden, so the exact const-qualified signatures matter.

#ifndef __MmmProcess_h
#define __MmmProcess_h

#include <pcl/MetaProcess.h>
#include <pcl/ProcessImplementation.h>

namespace pcl
{

// ----------------------------------------------------------------------------

/*!
 * \class MmmBlendProcess
 * \brief Meta-object for the MergeMosaic global-context blend process.
 *
 * Registers itself under the module singleton on construction (MetaObject
 * parent = pcl::Module). Global-only: CanProcessViews() is false and
 * PrefersGlobalExecution() is true.
 */
class MmmBlendProcess : public MetaProcess
{
public:

   MmmBlendProcess();

   IsoString              Id() const override;
   IsoString              Categories() const override;
   uint32                 Version() const override;
   String                 Description() const override;
   ProcessImplementation* Create() const override;
   ProcessImplementation* Clone( const ProcessImplementation& ) const override;
   bool                   CanProcessViews() const override;
   bool                   PrefersGlobalExecution() const override;
   ProcessInterface*      DefaultInterface() const override;
};

/*!
 * \brief The MergeMosaic process meta-object singleton.
 *
 * Instantiated once by InstallPixInsightModule(); non-owning global handle used
 * by the interface and instance code.
 */
extern MmmBlendProcess* TheMmmBlendProcess;

// ----------------------------------------------------------------------------

/*!
 * \class MmmBlendInstance
 * \brief A process instance for MmmBlendProcess.
 *
 * Task 1 carries no parameters and performs no work: CanExecuteGlobal() returns
 * true, CanExecuteOn() returns false (the process never runs per-view), and
 * ExecuteGlobal() is a successful no-op.
 */
class MmmBlendInstance : public ProcessImplementation
{
public:

   MmmBlendInstance( const MetaProcess* );
   MmmBlendInstance( const MmmBlendInstance& );

   void Assign( const ProcessImplementation& ) override;
   bool CanExecuteGlobal( String& whyNot ) const override;
   bool CanExecuteOn( const View& view, String& whyNot ) const override;
   bool ExecuteGlobal() override;
};

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmProcess_h
