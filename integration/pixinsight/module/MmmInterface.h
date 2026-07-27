// MmmInterface.h -- MergeMosaic ProcessInterface (Task 1: trivial skeleton).
//
// A static, instance-generating interface. Task 1 builds no child controls;
// Launch() succeeds without populating the (empty) Control that the interface
// is, per PCL_API_REFERENCE.md section 4. Real UI arrives in later tasks.

#ifndef __MmmInterface_h
#define __MmmInterface_h

#include <pcl/ProcessInterface.h>

namespace pcl
{

// ----------------------------------------------------------------------------

/*!
 * \class MmmBlendInterface
 * \brief The process interface (tool window) for MmmBlendProcess.
 *
 * Self-registers under pcl::Module on construction. Id() and Process() are the
 * only pure virtuals; NewProcess() makes it an instance generator so the core
 * can create/import MmmBlendInstance objects.
 */
class MmmBlendInterface : public ProcessInterface
{
public:

   MmmBlendInterface();

   IsoString              Id() const override;
   MetaProcess*           Process() const override;
   InterfaceFeatures      Features() const override;
   ProcessImplementation* NewProcess() const override;
   bool                   Launch( const MetaProcess&, const ProcessImplementation*,
                                  bool& dynamic, unsigned& flags ) override;
};

/*!
 * \brief The MergeMosaic interface singleton.
 *
 * Instantiated once by InstallPixInsightModule(); non-owning global handle.
 */
extern MmmBlendInterface* TheMmmBlendInterface;

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmInterface_h
