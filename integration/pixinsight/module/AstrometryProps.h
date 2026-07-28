// AstrometryProps.h -- extract a solved panel's plate solution as worker JSON (Task 4).
//
// A PixInsight astrometric solution lives in a view's properties under the
// "PCL:AstrometricSolution:" id prefix (PCL_API_REFERENCE.md section 6). For a
// Solved-mode IPC job the host forwards those raw properties verbatim in each
// PanelDesc.properties (PROTOCOL.md sections 6 & 11); the worker rebuilds its
// WCS model directly from them (mmm_core::astrometry). This helper serializes
// them into the exact serde JSON shape the worker parses: an array of
// XisfProperty { id, type_, value, location } objects, with value a
// PropertyValue variant.
//
// PCL signatures verified against /opt/PixInsight/include/pcl/{View,Property,
// Variant}.h; the JSON shape verified field-for-field against
// crates/mmm-core/src/ipc/protocol.rs + formats/mod.rs.

#ifndef __MmmAstrometryProps_h
#define __MmmAstrometryProps_h

#include <pcl/Property.h>
#include <pcl/View.h>

#include "third_party/json.hpp"

namespace pcl
{

/*!
 * \brief Serializes the "PCL:AstrometricSolution:*" entries of a PropertyArray
 * as the worker's XisfProperty JSON array.
 *
 * Keeps ids beginning "PCL:AstrometricSolution:" and maps each pcl::Variant
 * value to the matching PropertyValue JSON variant (vector -> F64Vec, matrix ->
 * F64Mat, string/time -> Str, float -> F64, integer/bool -> I64). type_ is set
 * to the XISF type name; location is always null (properties are read in
 * memory, not attachment-located). Properties carrying a non-finite float, and
 * properties of an unsupported Variant type, are skipped -- so the finite-float
 * precondition the worker's Init decoder enforces (PROTOCOL.md section 6)
 * always holds.
 *
 * The returned value is a JSON array suitable for a PanelDesc.properties field.
 * The \a view overload forwards \a view.Properties(); the PropertyArray overload
 * lets a caller pass properties read straight from a file
 * (FileFormatInstance::ReadImageProperties) so a Files-mode host can probe a
 * solved frame without a live view.
 */
nlohmann::json extract_astrometry_props( const PropertyArray& properties );
nlohmann::json extract_astrometry_props( const View& view );

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmAstrometryProps_h
