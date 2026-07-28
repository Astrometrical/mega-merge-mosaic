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

#include <pcl/Array.h>
#include <pcl/MetaParameter.h>
#include <pcl/MetaProcess.h>
#include <pcl/ProcessImplementation.h>
#include <pcl/String.h>

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

   // Parameter storage plumbing (dispatch by MetaParameter* pointer identity).
   void*     LockParameter( const MetaParameter* p, size_type tableRow ) override;
   bool      AllocateParameter( size_type sizeOrLength, const MetaParameter* p, size_type tableRow ) override;
   size_type ParameterLength( const MetaParameter* p, size_type tableRow ) const override;

private:

   // Per-instance parameter values. Booleans/enums MUST use pcl_bool/pcl_enum
   // (both int32-based) for ABI compatibility with the core, never C++
   // bool/enum. The two Array<String> members are the runtime row storage for
   // the inputImages/filePaths MetaTables (one String per row); the MetaString
   // columns index into them by tableRow.
   Array<String> p_viewIds;         // inputImages table rows (view ids)
   Array<String> p_filePaths;       // filePaths table rows (file paths)
   pcl_enum      p_inputSelect;     // Auto / Aligned / Solved
   String        p_sessionDir;      // *.mmm-session directory
   float         p_feather;         // feather ramp length, canvas px
   pcl_enum      p_blendMode;       // Feather / TwoBand / Pyramid
   int32         p_flatten;         // background-flatten polynomial order
   pcl_bool      p_flattenEnabled;  // whether flatten is applied
   int32         p_roi[ 4 ];        // ROI [x0,y0,x1,y1], full-res canvas coords
   pcl_bool      p_roiEnabled;      // whether the ROI is applied
   int32         p_downsample;      // 1 = full res, 8 = L8 preview
   pcl_bool      p_defectVeto;      // cross-panel defect veto
   int32         p_surfaceOrder;    // analyze surface-fit order
   int32         p_bandRows;        // output band granularity, rows

   friend class MmmBlendProcess;
   friend class MmmBlendInterface;
   // Task 5: the ExecuteGlobal orchestration reads the instance's parameters
   // directly to build the worker Init job (see MmmExecution.cpp).
   friend void run_blend( MmmBlendInstance& );
};

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmProcess_h
