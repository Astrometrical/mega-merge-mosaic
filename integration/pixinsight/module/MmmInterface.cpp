// MmmInterface.cpp -- MergeMosaic ProcessInterface implementation (Task 3).
//
// Builds the real control tree (deferred-init GUIData, per PCL_API_REFERENCE.md
// section 4) and wires every control to the interface's private working
// MmmBlendInstance (m_instance) using the direct OnXxx(handler, receiver)
// idiom -- there is no __CLASS_HANDLER macro in PCL. NewProcess() hands out
// copies of m_instance; ImportProcess() adopts a foreign instance's values
// into m_instance and refreshes the controls.
//
// Mutual exclusion of the Views/Files input (spec section 10.1): the
// interface never lets both p_viewIds and p_filePaths be non-empty at once.
// Switching the toggle, or adding to one side, clears the other side's array
// on the working instance; UpdateControls() re-syncs both radio buttons and
// both TreeBoxes' enabled state unconditionally, so the displayed state can
// never drift from the interface's own model even if the platform's native
// RadioButton exclusivity behaves differently than expected (unconfirmed
// against the reference doc -- see task-3-report.md).

#include "MmmInterface.h"
#include "MmmExecution.h"
#include "MmmIcon.h"

#include <pcl/Array.h>
#include <pcl/FileDialog.h>
#include <pcl/MultiViewSelectionDialog.h>
#include <pcl/View.h>

namespace pcl
{

// ----------------------------------------------------------------------------

MmmBlendInterface* TheMmmBlendInterface = nullptr;

// ----------------------------------------------------------------------------

MmmBlendInterface::MmmBlendInterface()
   : m_instance( TheMmmBlendProcess )
{
   // Constructing a ProcessInterface self-registers it under pcl::Module.
   TheMmmBlendInterface = this;
}

MmmBlendInterface::~MmmBlendInterface()
{
   delete GUI;
}

IsoString MmmBlendInterface::Id() const
{
   return "MegaMergeMosaic";
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
   return new MmmBlendInstance( m_instance );
}

bool MmmBlendInterface::ImportProcess( const ProcessImplementation& p )
{
   const MmmBlendInstance* instance = dynamic_cast<const MmmBlendInstance*>( &p );
   if ( instance == nullptr )
      return false;

   m_instance.Assign( *instance );

   // Re-derive the transient Views-vs-Files toggle from which side actually
   // carries data (there is no explicit mode member on the instance -- see
   // spec 10.1). Default to Views mode if both sides happen to be empty.
   m_viewsMode = !m_instance.p_viewIds.IsEmpty() || m_instance.p_filePaths.IsEmpty();

   // Nothing in Assign()/the copy constructor prevents a foreign (legacy or
   // scripted) instance from carrying BOTH arrays populated at once. Enforce
   // the same single-input-type invariant here that every other mutation
   // point (e_ModeClick, e_AddViewsClick, e_AddFilesClick) enforces: clear
   // whichever side m_viewsMode did NOT select.
   if ( m_viewsMode )
      m_instance.p_filePaths.Clear();
   else
      m_instance.p_viewIds.Clear();

   if ( GUI != nullptr )
      UpdateControls();

   return true;
}

bool MmmBlendInterface::Launch( const MetaProcess&, const ProcessImplementation*,
                                 bool& dynamic, unsigned& /*flags*/ )
{
   // Deferred initialization: build the control tree only the first time the
   // interface is actually shown. Per the ProcessInterface::Launch() doc, the
   // core calls ValidateProcess()/ImportProcess() itself right after a
   // successful Launch() from an existing instance, so this function does not
   // need to import `instance` explicitly.
   if ( GUI == nullptr )
   {
      GUI = new GUIData( *this );
      SetWindowTitle( "Mega Merge Mosaic" );
      UpdateControls();
   }

   dynamic = false;
   return true;
}

IsoString MmmBlendInterface::IconImageSVG() const
{
   return MMM_PROCESS_ICON_SVG;
}

// ----------------------------------------------------------------------------
// GUIData -- control tree construction + event wiring.
// ----------------------------------------------------------------------------

MmmBlendInterface::GUIData::GUIData( MmmBlendInterface& w )
{
   //
   // Input source: Views / Files toggle + the two list widgets.
   //
   ViewsMode_RadioButton.SetText( "Views" );
   ViewsMode_RadioButton.SetChecked();
   ViewsMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );

   FilesMode_RadioButton.SetText( "Files" );
   FilesMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );

   InputMode_Sizer.SetSpacing( 8 );
   InputMode_Sizer.Add( ViewsMode_RadioButton );
   InputMode_Sizer.Add( FilesMode_RadioButton );
   InputMode_Sizer.AddStretch();

   Views_TreeBox.SetNumberOfColumns( 1 );
   Views_TreeBox.SetHeaderText( 0, "View Id" );
   Views_TreeBox.EnableMultipleSelections();
   Views_TreeBox.SetScaledMinHeight( 100 );

   AddViews_PushButton.SetText( "Add Views…" );
   AddViews_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddViewsClick, w );

   RemoveView_PushButton.SetText( "Remove" );
   RemoveView_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveViewClick, w );

   ViewButtons_Sizer.SetSpacing( 6 );
   ViewButtons_Sizer.Add( AddViews_PushButton );
   ViewButtons_Sizer.Add( RemoveView_PushButton );
   ViewButtons_Sizer.AddStretch();

   Views_Sizer.SetSpacing( 4 );
   Views_Sizer.Add( Views_TreeBox );
   Views_Sizer.Add( ViewButtons_Sizer );

   Files_TreeBox.SetNumberOfColumns( 1 );
   Files_TreeBox.SetHeaderText( 0, "File Path" );
   Files_TreeBox.EnableMultipleSelections();
   Files_TreeBox.SetScaledMinHeight( 100 );

   AddFiles_PushButton.SetText( "Add Files…" );
   AddFiles_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddFilesClick, w );

   RemoveFile_PushButton.SetText( "Remove" );
   RemoveFile_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveFileClick, w );

   FileButtons_Sizer.SetSpacing( 6 );
   FileButtons_Sizer.Add( AddFiles_PushButton );
   FileButtons_Sizer.Add( RemoveFile_PushButton );
   FileButtons_Sizer.AddStretch();

   Files_Sizer.SetSpacing( 4 );
   Files_Sizer.Add( Files_TreeBox );
   Files_Sizer.Add( FileButtons_Sizer );

   InputSource_Sizer.SetSpacing( 6 );
   InputSource_Sizer.Add( InputMode_Sizer );
   InputSource_Sizer.Add( Views_Sizer );
   InputSource_Sizer.Add( Files_Sizer );

   InputSource_GroupBox.SetTitle( "Input Source" );
   InputSource_GroupBox.SetSizer( InputSource_Sizer );

   //
   // Session directory.
   //
   SessionDir_Label.SetText( "Session directory:" );

   SessionDir_Edit.OnEditCompleted( (Edit::edit_event_handler)&MmmBlendInterface::e_SessionDirEditCompleted, w );

   SessionDir_PushButton.SetText( "…" );
   SessionDir_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_SessionDirBrowseClick, w );

   SessionDir_Sizer.SetSpacing( 6 );
   SessionDir_Sizer.Add( SessionDir_Label );
   SessionDir_Sizer.Add( SessionDir_Edit, 100 );
   SessionDir_Sizer.Add( SessionDir_PushButton );

   //
   // Input override (advanced): Auto / Aligned / Solved.
   // Element order MUST match MmmInputSelectParameter (Auto=0/Aligned=1/Solved=2).
   //
   InputSelect_Label.SetText( "Input:" );

   InputSelect_ComboBox.AddItem( "Auto" );
   InputSelect_ComboBox.AddItem( "Aligned" );
   InputSelect_ComboBox.AddItem( "Solved" );
   InputSelect_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_InputSelectItemSelected, w );

   InputSelect_Sizer.SetSpacing( 6 );
   InputSelect_Sizer.Add( InputSelect_Label );
   InputSelect_Sizer.Add( InputSelect_ComboBox );
   InputSelect_Sizer.AddStretch();

   //
   // Blend parameters.
   //
   BlendMode_Label.SetText( "Blend mode:" );

   // Element order MUST match MmmBlendModeParameter (Feather=0/TwoBand=1/Pyramid=2).
   BlendMode_ComboBox.AddItem( "Feather" );
   BlendMode_ComboBox.AddItem( "TwoBand" );
   BlendMode_ComboBox.AddItem( "Pyramid" );
   BlendMode_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_BlendModeItemSelected, w );

   BlendMode_Sizer.SetSpacing( 6 );
   BlendMode_Sizer.Add( BlendMode_Label );
   BlendMode_Sizer.Add( BlendMode_ComboBox );
   BlendMode_Sizer.AddStretch();

   Feather_NumericControl.label.SetText( "Feather:" );
   Feather_NumericControl.SetInteger();
   Feather_NumericControl.SetRange( 1, 1024 );
   Feather_NumericControl.OnValueUpdated( (NumericEdit::value_event_handler)&MmmBlendInterface::e_FeatherValueUpdated, w );

   SurfaceOrder_Label.SetText( "Surface fit order:" );
   SurfaceOrder_SpinBox.SetRange( 0, 8 );
   SurfaceOrder_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_SurfaceOrderValueUpdated, w );

   SurfaceOrder_Sizer.SetSpacing( 6 );
   SurfaceOrder_Sizer.Add( SurfaceOrder_Label );
   SurfaceOrder_Sizer.Add( SurfaceOrder_SpinBox );
   SurfaceOrder_Sizer.AddStretch();

   BandRows_Label.SetText( "Band rows:" );
   BandRows_SpinBox.SetRange( 1, 65536 );
   BandRows_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_BandRowsValueUpdated, w );

   BandRows_Sizer.SetSpacing( 6 );
   BandRows_Sizer.Add( BandRows_Label );
   BandRows_Sizer.Add( BandRows_SpinBox );
   BandRows_Sizer.AddStretch();

   DefectVeto_CheckBox.SetText( "Cross-panel defect veto" );
   DefectVeto_CheckBox.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_DefectVetoClick, w );

   FlattenEnabled_CheckBox.SetText( "Flatten background, order:" );
   FlattenEnabled_CheckBox.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_FlattenEnabledClick, w );

   FlattenOrder_SpinBox.SetRange( 0, 8 );
   FlattenOrder_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_FlattenOrderValueUpdated, w );

   Flatten_Sizer.SetSpacing( 6 );
   Flatten_Sizer.Add( FlattenEnabled_CheckBox );
   Flatten_Sizer.Add( FlattenOrder_SpinBox );
   Flatten_Sizer.AddStretch();

   BlendParams_Sizer.SetSpacing( 6 );
   BlendParams_Sizer.Add( BlendMode_Sizer );
   BlendParams_Sizer.Add( Feather_NumericControl );
   BlendParams_Sizer.Add( SurfaceOrder_Sizer );
   BlendParams_Sizer.Add( BandRows_Sizer );
   BlendParams_Sizer.Add( DefectVeto_CheckBox );
   BlendParams_Sizer.Add( Flatten_Sizer );

   BlendParams_GroupBox.SetTitle( "Blend Parameters" );
   BlendParams_GroupBox.SetSizer( BlendParams_Sizer );

   //
   // Progress + cancel. Task 5 wires the live progress feed and enables
   // Cancel_PushButton; here the control exists but starts disabled.
   //
   Cancel_PushButton.SetText( "Cancel" );
   Cancel_PushButton.Disable();
   Cancel_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_CancelClick, w );

   Progress_Sizer.SetSpacing( 6 );
   Progress_Sizer.Add( Progress_Label, 100 );
   Progress_Sizer.Add( Cancel_PushButton );

   Progress_GroupBox.SetTitle( "Progress" );
   Progress_GroupBox.SetSizer( Progress_Sizer );

   //
   // Top-level layout.
   //
   Global_Sizer.SetMargin( 8 );
   Global_Sizer.SetSpacing( 8 );
   Global_Sizer.Add( InputSource_GroupBox );
   Global_Sizer.Add( SessionDir_Sizer );
   Global_Sizer.Add( InputSelect_Sizer );
   Global_Sizer.Add( BlendParams_GroupBox );
   Global_Sizer.Add( Progress_GroupBox );

   w.SetSizer( Global_Sizer );
   w.AdjustToContents();
}

// ----------------------------------------------------------------------------
// Control <-> instance sync.
// ----------------------------------------------------------------------------

void MmmBlendInterface::UpdateControls()
{
   GUI->ViewsMode_RadioButton.SetChecked( m_viewsMode );
   GUI->FilesMode_RadioButton.SetChecked( !m_viewsMode );
   UpdateInputModeControls();

   PopulateViewsTreeBox();
   PopulateFilesTreeBox();

   GUI->SessionDir_Edit.SetText( m_instance.p_sessionDir );

   GUI->InputSelect_ComboBox.SetCurrentItem( m_instance.p_inputSelect );
   GUI->BlendMode_ComboBox.SetCurrentItem( m_instance.p_blendMode );

   GUI->Feather_NumericControl.SetValue( m_instance.p_feather );
   GUI->SurfaceOrder_SpinBox.SetValue( m_instance.p_surfaceOrder );
   GUI->BandRows_SpinBox.SetValue( m_instance.p_bandRows );

   GUI->DefectVeto_CheckBox.SetChecked( m_instance.p_defectVeto );

   GUI->FlattenEnabled_CheckBox.SetChecked( m_instance.p_flattenEnabled );
   GUI->FlattenOrder_SpinBox.SetValue( m_instance.p_flatten );
   UpdateFlattenControls();
}

void MmmBlendInterface::UpdateInputModeControls()
{
   GUI->Views_TreeBox.Enable( m_viewsMode );
   GUI->AddViews_PushButton.Enable( m_viewsMode );
   GUI->RemoveView_PushButton.Enable( m_viewsMode );

   GUI->Files_TreeBox.Enable( !m_viewsMode );
   GUI->AddFiles_PushButton.Enable( !m_viewsMode );
   GUI->RemoveFile_PushButton.Enable( !m_viewsMode );
}

void MmmBlendInterface::UpdateFlattenControls()
{
   GUI->FlattenOrder_SpinBox.Enable( m_instance.p_flattenEnabled );
}

void MmmBlendInterface::PopulateViewsTreeBox()
{
   GUI->Views_TreeBox.Clear();
   for ( const String& id : m_instance.p_viewIds )
   {
      TreeBox::Node* node = new TreeBox::Node( GUI->Views_TreeBox );
      node->SetText( 0, id );
   }
}

void MmmBlendInterface::PopulateFilesTreeBox()
{
   GUI->Files_TreeBox.Clear();
   for ( const String& path : m_instance.p_filePaths )
   {
      TreeBox::Node* node = new TreeBox::Node( GUI->Files_TreeBox );
      node->SetText( 0, path );
   }
}

// ----------------------------------------------------------------------------
// Event handlers.
// ----------------------------------------------------------------------------

void MmmBlendInterface::e_ModeClick( Button& sender, bool checked )
{
   // Only react to the button that has just turned ON; this keeps behavior
   // correct whether or not the platform's RadioButton grouping also fires a
   // (checked=false) event on the sibling button.
   if ( !checked )
      return;

   if ( &sender == &GUI->ViewsMode_RadioButton )
   {
      m_viewsMode = true;
      m_instance.p_filePaths.Clear();
   }
   else if ( &sender == &GUI->FilesMode_RadioButton )
   {
      m_viewsMode = false;
      m_instance.p_viewIds.Clear();
   }

   UpdateControls();
}

void MmmBlendInterface::e_AddViewsClick( Button&, bool )
{
   MultiViewSelectionDialog d;
   if ( d.Execute() == StdDialogCode::Ok )
   {
      for ( const View& v : d.Views() )
      {
         String id( v.FullId().UTF8ToUTF16() );
         bool exists = false;
         for ( const String& s : m_instance.p_viewIds )
            if ( s == id )
            {
               exists = true;
               break;
            }
         if ( !exists )
            m_instance.p_viewIds.Add( id );
      }

      // Adding views implies Views mode (mutual exclusion, spec 10.1).
      m_instance.p_filePaths.Clear();
      m_viewsMode = true;

      UpdateControls();
   }
}

void MmmBlendInterface::e_RemoveViewClick( Button&, bool )
{
   IndirectArray<TreeBox::Node> selected = GUI->Views_TreeBox.SelectedNodes();

   Array<int> rows;
   for ( TreeBox::Node* node : selected )
      rows.Add( GUI->Views_TreeBox.ChildIndex( node ) );
   rows.Sort();

   // Remove from the highest index down so earlier indices stay valid, and
   // keep p_viewIds in lockstep with the TreeBox's top-level row order.
   for ( int i = int( rows.Length() ) - 1; i >= 0; --i )
   {
      int row = rows[i];
      if ( row >= 0 && size_type( row ) < m_instance.p_viewIds.Length() )
         m_instance.p_viewIds.Remove( m_instance.p_viewIds.Begin() + row );
   }

   UpdateControls();
}

void MmmBlendInterface::e_AddFilesClick( Button&, bool )
{
   OpenFileDialog d;
   d.LoadImageFilters();
   d.EnableMultipleSelections();
   if ( d.Execute() )
   {
      for ( const String& f : d.FileNames() )
      {
         bool exists = false;
         for ( const String& s : m_instance.p_filePaths )
            if ( s == f )
            {
               exists = true;
               break;
            }
         if ( !exists )
            m_instance.p_filePaths.Add( f );
      }

      // Adding files implies Files mode (mutual exclusion, spec 10.1).
      m_instance.p_viewIds.Clear();
      m_viewsMode = false;

      UpdateControls();
   }
}

void MmmBlendInterface::e_RemoveFileClick( Button&, bool )
{
   IndirectArray<TreeBox::Node> selected = GUI->Files_TreeBox.SelectedNodes();

   Array<int> rows;
   for ( TreeBox::Node* node : selected )
      rows.Add( GUI->Files_TreeBox.ChildIndex( node ) );
   rows.Sort();

   for ( int i = int( rows.Length() ) - 1; i >= 0; --i )
   {
      int row = rows[i];
      if ( row >= 0 && size_type( row ) < m_instance.p_filePaths.Length() )
         m_instance.p_filePaths.Remove( m_instance.p_filePaths.Begin() + row );
   }

   UpdateControls();
}

void MmmBlendInterface::e_SessionDirEditCompleted( Edit& sender )
{
   m_instance.p_sessionDir = sender.Text();
}

void MmmBlendInterface::e_SessionDirBrowseClick( Button&, bool )
{
   GetDirectoryDialog d;
   d.SetCaption( "Mega Merge Mosaic: Select Session Directory" );
   if ( d.Execute() )
   {
      m_instance.p_sessionDir = d.Directory();
      UpdateControls();
   }
}

void MmmBlendInterface::e_InputSelectItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_inputSelect = pcl_enum( itemIndex );
}

void MmmBlendInterface::e_BlendModeItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_blendMode = pcl_enum( itemIndex );
}

void MmmBlendInterface::e_FeatherValueUpdated( NumericEdit&, double value )
{
   m_instance.p_feather = int32( value );
}

void MmmBlendInterface::e_SurfaceOrderValueUpdated( SpinBox&, int value )
{
   m_instance.p_surfaceOrder = int32( value );
}

void MmmBlendInterface::e_BandRowsValueUpdated( SpinBox&, int value )
{
   m_instance.p_bandRows = int32( value );
}

void MmmBlendInterface::e_DefectVetoClick( Button&, bool checked )
{
   m_instance.p_defectVeto = checked;
}

void MmmBlendInterface::e_FlattenEnabledClick( Button&, bool checked )
{
   m_instance.p_flattenEnabled = checked;
   UpdateFlattenControls();
}

void MmmBlendInterface::e_FlattenOrderValueUpdated( SpinBox&, int value )
{
   m_instance.p_flatten = int32( value );
}

void MmmBlendInterface::e_CancelClick( Button&, bool )
{
   // Ask the running blend to stop (spec section 15). The button is enabled only
   // for the duration of a run (SetBlendRunning); request_cancel() is a no-op if
   // nothing is running. The synchronous v1 delivers this click via the
   // ProcessEvents() pump inside the run's progress callback.
   request_cancel();
}

// ----------------------------------------------------------------------------
// Task 5 run-state hooks (called from MmmExecution.cpp on the execute thread).
// ----------------------------------------------------------------------------

void MmmBlendInterface::SetBlendRunning( bool running )
{
   if ( GUI == nullptr )
      return;
   GUI->Cancel_PushButton.Enable( running );
   if ( !running )
      GUI->Progress_Label.Clear();
}

void MmmBlendInterface::SetProgressText( const String& text )
{
   if ( GUI == nullptr )
      return;
   GUI->Progress_Label.SetText( text );
}

// ----------------------------------------------------------------------------

} // namespace pcl
